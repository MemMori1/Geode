//! hex 字节模式：解析带半字节通配的字符串并编译为掩码序列。
//!
//! 语法见 `08-search-subsystem.md` §4.1：空白可选，`??` 为整字节通配，
//! `?F` / `F?` 为半字节通配。编译产物是不做任何分配的单字节掩码比较，
//! 适合逐块流式扫描。

use crate::SearchError;

/// 一个带掩码的字节。
///
/// `value & mask == byte & mask` 即视为匹配。`mask == 0xFF` 表示精确字节，
/// `mask == 0x00` 表示任意字节。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MaskByte {
    /// 期望值（只使用 `mask` 为 1 的位）。
    pub value: u8,
    /// 必须匹配的位；0 表示通配。
    pub mask: u8,
}

impl MaskByte {
    /// 构造任意掩码字节。
    pub const fn new(value: u8, mask: u8) -> Self {
        Self { value, mask }
    }

    /// 精确字节（`mask == 0xFF`）。
    pub const fn exact(value: u8) -> Self {
        Self { value, mask: 0xFF }
    }

    /// 任意字节（`??`）。
    pub const fn any() -> Self {
        Self { value: 0, mask: 0 }
    }

    /// 单字节是否满足本掩码。
    #[inline]
    pub const fn matches(self, byte: u8) -> bool {
        // 同时掩掉 value：允许调用方用 `MaskByte::new(0xFF, 0x0F)` 这类
        // 未预先归一的构造方式，语义仍等价于“只比较 mask 为 1 的位”。
        byte & self.mask == self.value & self.mask
    }
}

/// 解析后的 hex 模式。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HexPattern {
    /// 编译后的掩码字节序列，顺序与输入一致。
    pub bytes: Vec<MaskByte>,
}

impl HexPattern {
    /// 解析 hex 字符串。
    ///
    /// 接受 `"48 65 ?? 6C"`、`"4865??6C"` 等形式；十六进制字符大小写不敏感，
    /// 空白可出现在任意位置。非法输入（奇数个字符、非 hex 字符、空串）以及
    /// 没有任何固定位的模式（如 `"?? ??"`）返回 [`SearchError::BadPattern`]。
    pub fn parse(input: &str) -> Result<Self, SearchError> {
        let cleaned: String = input.chars().filter(|c| !c.is_whitespace()).collect();
        if cleaned.is_empty() {
            return Err(SearchError::bad_pattern("hex 模式为空"));
        }
        let chars: Vec<char> = cleaned.chars().collect();
        if chars.len() % 2 != 0 {
            return Err(SearchError::bad_pattern(format!(
                "hex 模式字符数为奇数（{}），每个字节需要两位",
                chars.len()
            )));
        }

        let mut bytes = Vec::with_capacity(chars.len() / 2);
        for pair in chars.chunks_exact(2) {
            let hi = nibble(pair[0])?;
            let lo = nibble(pair[1])?;
            bytes.push(match (hi, lo) {
                (None, None) => MaskByte::any(),
                (None, Some(low)) => MaskByte::new(low, 0x0F),
                (Some(high), None) => MaskByte::new(high << 4, 0xF0),
                (Some(high), Some(low)) => MaskByte::exact((high << 4) | low),
            });
        }

        if bytes.iter().all(|byte| byte.mask == 0) {
            return Err(SearchError::bad_pattern(
                "模式没有任何固定字节（E7102：请至少指定一个固定字节或半字节）",
            ));
        }
        Ok(Self { bytes })
    }

    /// 从原始字节构造全精确模式。
    ///
    /// 空切片会产生空模式；[`crate::search`] 会拒绝空模式。
    pub fn from_bytes(input: &[u8]) -> Self {
        Self {
            bytes: input.iter().copied().map(MaskByte::exact).collect(),
        }
    }

    /// 字节数。
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    /// 是否为空模式。
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// 是否包含通配位。
    pub fn has_wildcards(&self) -> bool {
        self.bytes.iter().any(|byte| byte.mask != 0xFF)
    }

    /// 在 `haystack[pos..]` 处是否匹配本模式。
    ///
    /// 越界（`pos + len > haystack.len()`）返回 `false`，不 panic。
    #[inline]
    pub fn matches_at(&self, haystack: &[u8], pos: usize) -> bool {
        let Some(end) = pos.checked_add(self.bytes.len()) else {
            return false;
        };
        if end > haystack.len() {
            return false;
        }
        self.bytes
            .iter()
            .enumerate()
            .all(|(index, pattern)| pattern.matches(haystack[pos + index]))
    }
}

fn nibble(c: char) -> Result<Option<u8>, SearchError> {
    if c == '?' {
        return Ok(None);
    }
    c.to_digit(16)
        .map(|digit| Some(digit as u8))
        .ok_or_else(|| SearchError::bad_pattern(format!("非法 hex 字符 {c:?}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_exact_bytes_with_optional_spaces() {
        let pattern = HexPattern::parse("48 65 6C 6C 6F").expect("合法模式");
        assert_eq!(
            pattern.bytes,
            vec![
                MaskByte::exact(0x48),
                MaskByte::exact(0x65),
                MaskByte::exact(0x6C),
                MaskByte::exact(0x6C),
                MaskByte::exact(0x6F),
            ]
        );
        assert_eq!(HexPattern::parse("48656C6C6F").expect("紧凑写法"), pattern);
        assert!(!pattern.has_wildcards());
    }

    #[test]
    fn parses_nibble_wildcards() {
        let pattern = HexPattern::parse("?F 4? ??").expect("合法模式");
        assert_eq!(
            pattern.bytes,
            vec![
                MaskByte::new(0x0F, 0x0F),
                MaskByte::new(0x40, 0xF0),
                MaskByte::any(),
            ]
        );
        assert!(pattern.bytes[0].matches(0xAF));
        assert!(!pattern.bytes[0].matches(0xA0));
        assert!(pattern.bytes[1].matches(0x4C));
        assert!(!pattern.bytes[1].matches(0x5C));
    }

    #[test]
    fn parses_case_insensitively() {
        let lower = HexPattern::parse("de ad be ef").expect("小写");
        let upper = HexPattern::parse("DEADBEEF").expect("大写");
        assert_eq!(lower, upper);
    }

    #[test]
    fn rejects_malformed_patterns() {
        for input in ["", "   ", "4", "48 6", "GG", "0x48", "48-65"] {
            let err = HexPattern::parse(input).expect_err("应拒绝");
            assert_eq!(err.code(), "E7102");
            assert!(matches!(err, SearchError::BadPattern { .. }));
        }
    }

    #[test]
    fn rejects_pattern_without_any_fixed_bit() {
        for input in ["??", "?? ??", "? ?"] {
            let err = HexPattern::parse(input).expect_err("全通配应拒绝");
            assert_eq!(err.code(), "E7102");
        }
        // 只有半字节固定也算有信息量，不能拒绝。
        assert!(HexPattern::parse("?0 ??").is_ok());
        assert!(HexPattern::parse("0? ??").is_ok());
    }

    #[test]
    fn matches_at_respects_every_mask() {
        let pattern = HexPattern::parse("4? ?? ?F").expect("合法模式");
        assert!(pattern.matches_at(&[0x4A, 0x00, 0x1F, 0xFF], 0));
        assert!(!pattern.matches_at(&[0x5A, 0x00, 0x1F], 0));
        assert!(!pattern.matches_at(&[0x4A, 0x00, 0x20], 0));
    }
}
