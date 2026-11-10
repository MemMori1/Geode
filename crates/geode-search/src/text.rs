//! 文本查询：把查询串编码成目标字节序列，再交给流式字节扫描。
//!
//! 采用设计文档的方案 B：不把数据解码成 UTF-8，而是把查询串编码成目标
//! 编码。这样即使数据在任意位置截断/含非法序列，也不会产生解码错误。

use crate::SearchError;

/// 文本查询的目标编码。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TextEncoding {
    /// UTF-8。
    Utf8,
    /// UTF-16 小端。
    Utf16Le,
    /// UTF-16 大端。
    Utf16Be,
    /// 7 位 ASCII。
    Ascii,
    /// ISO-8859-1（Latin-1），每个字符映射为单字节 0x00..=0xFF。
    Latin1,
}

impl TextEncoding {
    /// 把查询串编码为 needle 字节。
    ///
    /// 空串、目标编码无法表示的字符（ASCII 遇到非 ASCII、Latin-1 遇到
    /// U+0100 以上字符）返回 [`SearchError::BadPattern`]。不写入 BOM，
    /// 需要 BOM 时用 [`TextEncoding::needle_bytes_with_bom`]。
    pub fn needle_bytes(self, text: &str) -> Result<Vec<u8>, SearchError> {
        self.needle_bytes_with_bom(text, false)
    }

    /// 把查询串编码为 needle 字节，可选写入 BOM。
    ///
    /// 只有 UTF-8（`EF BB BF`）和 UTF-16（`FF FE` / `FE FF`）支持 BOM；
    /// ASCII/Latin-1 请求 BOM 返回 [`SearchError::BadPattern`]。
    pub fn needle_bytes_with_bom(self, text: &str, bom: bool) -> Result<Vec<u8>, SearchError> {
        if text.is_empty() {
            return Err(SearchError::bad_pattern("文本查询为空"));
        }
        match self {
            Self::Utf8 => {
                let mut needle = Vec::with_capacity(text.len() + 3);
                if bom {
                    needle.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
                }
                needle.extend_from_slice(text.as_bytes());
                Ok(needle)
            }
            Self::Utf16Le | Self::Utf16Be => {
                let mut needle = Vec::with_capacity(2 + text.len() * 2);
                if bom {
                    needle.extend_from_slice(match self {
                        Self::Utf16Le => &[0xFF, 0xFE],
                        _ => &[0xFE, 0xFF],
                    });
                }
                for unit in text.encode_utf16() {
                    let bytes = match self {
                        Self::Utf16Le => unit.to_le_bytes(),
                        _ => unit.to_be_bytes(),
                    };
                    needle.extend_from_slice(&bytes);
                }
                Ok(needle)
            }
            Self::Ascii => {
                if bom {
                    return Err(SearchError::bad_pattern("ASCII 编码不支持 BOM"));
                }
                text.chars()
                    .map(|ch| {
                        let code = ch as u32;
                        if code <= 0x7F {
                            Ok(code as u8)
                        } else {
                            Err(SearchError::bad_pattern(format!(
                                "字符 {ch:?} 不能编码为 ASCII"
                            )))
                        }
                    })
                    .collect()
            }
            Self::Latin1 => {
                if bom {
                    return Err(SearchError::bad_pattern("Latin-1 编码不支持 BOM"));
                }
                text.chars()
                    .map(|ch| {
                        let code = ch as u32;
                        if code <= 0xFF {
                            Ok(code as u8)
                        } else {
                            Err(SearchError::bad_pattern(format!(
                                "字符 {ch:?} 不能编码为 Latin-1"
                            )))
                        }
                    })
                    .collect()
            }
        }
    }

    /// 编码名称，用于日志与 UI。
    pub const fn name(self) -> &'static str {
        match self {
            Self::Utf8 => "UTF-8",
            Self::Utf16Le => "UTF-16LE",
            Self::Utf16Be => "UTF-16BE",
            Self::Ascii => "ASCII",
            Self::Latin1 => "Latin-1",
        }
    }
}

/// 文本搜索查询。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextQuery {
    /// 查询文本。
    pub text: String,
    /// 目标编码。
    pub encoding: TextEncoding,
    /// 是否在 needle 前写入编码 BOM。
    pub bom: bool,
}

impl TextQuery {
    /// 构造不带 BOM 的文本查询。
    pub fn new(text: impl Into<String>, encoding: TextEncoding) -> Self {
        Self {
            text: text.into(),
            encoding,
            bom: false,
        }
    }

    /// 设置是否写入 BOM。
    pub fn with_bom(mut self, bom: bool) -> Self {
        self.bom = bom;
        self
    }

    /// 编码为 needle 字节。
    pub fn needle_bytes(&self) -> Result<Vec<u8>, SearchError> {
        self.encoding.needle_bytes_with_bom(&self.text, self.bom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_utf8_and_ascii() {
        assert_eq!(
            TextEncoding::Utf8.needle_bytes("hé").expect("UTF-8"),
            "hé".as_bytes()
        );
        assert_eq!(
            TextEncoding::Ascii.needle_bytes("ABC").expect("ASCII"),
            b"ABC"
        );
        assert!(TextEncoding::Ascii.needle_bytes("hé").is_err());
    }

    #[test]
    fn encodes_latin1_and_rejects_wide_chars() {
        assert_eq!(
            TextEncoding::Latin1.needle_bytes("café").expect("Latin-1"),
            vec![b'c', b'a', b'f', 0xE9]
        );
        assert!(TextEncoding::Latin1.needle_bytes("€").is_err());
    }

    #[test]
    fn encodes_utf16_both_endiannesses() {
        let le = TextEncoding::Utf16Le.needle_bytes("Aé").expect("LE");
        assert_eq!(le, vec![0x41, 0x00, 0xE9, 0x00]);
        let be = TextEncoding::Utf16Be.needle_bytes("Aé").expect("BE");
        assert_eq!(be, vec![0x00, 0x41, 0x00, 0xE9]);
    }

    #[test]
    fn writes_optional_bom() {
        let utf8 = TextEncoding::Utf8
            .needle_bytes_with_bom("A", true)
            .expect("UTF-8 BOM");
        assert_eq!(utf8, vec![0xEF, 0xBB, 0xBF, b'A']);
        let le = TextEncoding::Utf16Le
            .needle_bytes_with_bom("A", true)
            .expect("UTF-16LE BOM");
        assert_eq!(le, vec![0xFF, 0xFE, 0x41, 0x00]);
        let be = TextEncoding::Utf16Be
            .needle_bytes_with_bom("A", true)
            .expect("UTF-16BE BOM");
        assert_eq!(be, vec![0xFE, 0xFF, 0x00, 0x41]);
    }

    #[test]
    fn rejects_empty_text_and_unsupported_bom() {
        assert!(TextEncoding::Utf8.needle_bytes("").is_err());
        assert!(
            TextEncoding::Ascii
                .needle_bytes_with_bom("A", true)
                .is_err()
        );
        assert!(
            TextEncoding::Latin1
                .needle_bytes_with_bom("A", true)
                .is_err()
        );
    }

    #[test]
    fn text_query_delegates_to_encoding() {
        let query = TextQuery::new("AB", TextEncoding::Utf16Le).with_bom(true);
        assert_eq!(
            query.needle_bytes().expect("编码"),
            vec![0xFF, 0xFE, b'A', 0x00, b'B', 0x00]
        );
        assert_eq!(query.encoding.name(), "UTF-16LE");
    }
}
