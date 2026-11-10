//! 正则查询：基于 `regex::bytes` 的线性时间字节正则。
//!
//! `regex` crate 本身不支持反向引用和环视。为了让错误在构造阶段就明确
//! 暴露，本模块先做一次轻量语法预检，命中这些结构时返回
//! [`SearchError::UnsupportedRegex`]；其余语法错误返回
//! [`SearchError::InvalidRegex`]（均复用 E7103，不新增错误码）。

use regex::bytes::{Regex, RegexBuilder};

use crate::SearchError;

/// 字节正则查询。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegexQuery {
    /// 正则源串（Rust `regex` 语法）。
    pub pattern: String,
    /// `(?i)` 等价的大小写不敏感。
    pub case_insensitive: bool,
    /// `(?m)` 等价的多行模式：`^`/`$` 匹配行边界。
    pub multi_line: bool,
    /// `(?s)` 等价的 `.` 匹配换行。
    pub dot_matches_new_line: bool,
    /// 是否允许重叠匹配；默认 `false`（与 regex 的迭代语义一致）。
    pub overlapping: bool,
}

impl RegexQuery {
    /// 用默认选项构造正则查询。
    pub fn new(pattern: impl Into<String>) -> Self {
        Self {
            pattern: pattern.into(),
            case_insensitive: false,
            multi_line: false,
            dot_matches_new_line: false,
            overlapping: false,
        }
    }

    /// 设置大小写不敏感。
    pub fn with_case_insensitive(mut self, yes: bool) -> Self {
        self.case_insensitive = yes;
        self
    }

    /// 设置多行模式。
    pub fn with_multi_line(mut self, yes: bool) -> Self {
        self.multi_line = yes;
        self
    }

    /// 设置 `.` 是否匹配换行。
    pub fn with_dot_matches_new_line(mut self, yes: bool) -> Self {
        self.dot_matches_new_line = yes;
        self
    }

    /// 设置是否允许重叠匹配。
    pub fn with_overlapping(mut self, yes: bool) -> Self {
        self.overlapping = yes;
        self
    }

    /// 编译正则。
    pub(crate) fn compile(&self) -> Result<Regex, SearchError> {
        if self.pattern.is_empty() {
            return Err(SearchError::bad_pattern("正则模式为空"));
        }
        if let Some(reason) = forbidden_construct(&self.pattern) {
            return Err(SearchError::unsupported_regex(format!(
                "{reason}（regex crate 不支持，无法流式扫描）"
            )));
        }
        let mut builder = RegexBuilder::new(&self.pattern);
        builder
            .case_insensitive(self.case_insensitive)
            .multi_line(self.multi_line)
            .dot_matches_new_line(self.dot_matches_new_line);
        builder.build().map_err(|err| {
            let text = err.to_string();
            let lower = text.to_ascii_lowercase();
            if lower.contains("backreference") || lower.contains("look-around") {
                SearchError::unsupported_regex(text)
            } else {
                SearchError::invalid_regex(text)
            }
        })
    }
}

/// 轻量预检：返回第一个不支持结构的中文说明。
///
/// 只处理“结构”层面，不试图替代 `regex` 的语法解析；`\Q...\E` 等引用
/// 区段可能被保守地误报，但不会漏报常见的反向引用/环视写法。
fn forbidden_construct(pattern: &str) -> Option<&'static str> {
    let bytes = pattern.as_bytes();
    let mut index = 0usize;
    let mut in_class = false;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => {
                if index + 1 < bytes.len() {
                    let next = bytes[index + 1];
                    if !in_class {
                        if next.is_ascii_digit() && next != b'0' {
                            return Some("反向引用");
                        }
                        if matches!(next, b'k' | b'g') {
                            let after = bytes.get(index + 2).copied();
                            if matches!(after, Some(b'<') | Some(b'\'') | Some(b'{')) {
                                return Some("反向引用");
                            }
                        }
                    }
                    index += 2;
                } else {
                    index += 1;
                }
            }
            b'[' if !in_class => {
                in_class = true;
                index += 1;
            }
            b']' if in_class => {
                in_class = false;
                index += 1;
            }
            b'(' if !in_class => {
                let rest = &bytes[index..];
                if rest.starts_with(b"(?=")
                    || rest.starts_with(b"(?!")
                    || rest.starts_with(b"(?<=")
                    || rest.starts_with(b"(?<!")
                {
                    return Some("环视（lookaround）");
                }
                if rest.starts_with(b"(?P=") {
                    return Some("反向引用");
                }
                index += 1;
            }
            _ => index += 1,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiles_and_matches_bytes() {
        let regex = RegexQuery::new(r"ab\d+")
            .with_case_insensitive(true)
            .compile()
            .expect("合法正则");
        let found = regex.find(b"xxAB123yy").expect("应命中");
        assert_eq!((found.start(), found.end()), (2, 7));
    }

    #[test]
    fn rejects_lookaround_variants() {
        for pattern in [r"a(?=b)", r"a(?!b)", r"(?<=a)b", r"(?<!a)b"] {
            let err = RegexQuery::new(pattern).compile().expect_err("应拒绝");
            assert!(matches!(err, SearchError::UnsupportedRegex { .. }));
            assert_eq!(err.code(), "E7103");
        }
    }

    #[test]
    fn rejects_backreferences() {
        for pattern in [r"(a)\1", r"(?P<x>a)(?P=x)", r"(?<x>a)\k<x>"] {
            let err = RegexQuery::new(pattern).compile().expect_err("应拒绝");
            assert!(matches!(err, SearchError::UnsupportedRegex { .. }));
        }
    }

    #[test]
    fn reports_invalid_syntax_and_empty_pattern() {
        let err = RegexQuery::new("(").compile().expect_err("语法错误");
        assert!(matches!(err, SearchError::InvalidRegex { .. }));
        assert_eq!(err.code(), "E7103");
        assert!(RegexQuery::new("").compile().is_err());
    }

    #[test]
    fn lookaround_like_text_in_class_is_allowed() {
        let regex = RegexQuery::new(r"[(?=]").compile().expect("字符类");
        assert!(regex.is_match(b"("));
    }
}
