//! 字符串提取：ASCII 与 UTF-16LE，跨 1 MiB 块边界不丢串。
//!
//! DESIGN: 10 §4.7。只接受可打印 ASCII `0x20..=0x7E` 与 `\t`；UTF-16LE 只认
//! "低字节可打印、高字节为 0" 的代码单元，因此不会把二进制噪声当文本。
//! 单条串最长 1 MiB 字符，超过就切开，避免全可打印文件把内存吃满。

use std::fmt;

/// 默认最短字符串长度。
pub const DEFAULT_MIN_LEN: usize = 4;
/// 默认最多返回的字符串条数。
pub const DEFAULT_MAX_ENTRIES: usize = 100_000;
/// 单条字符串的最大字符数（防御性上限，超出后切成新串）。
pub const MAX_STRING_CHARS: usize = 1 << 20;

/// 字符串编码。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StringEncoding {
    /// 单字节 ASCII。
    Ascii,
    /// UTF-16 小端（仅 ASCII 码点）。
    Utf16Le,
}

impl StringEncoding {
    /// 稳定名字（用于报告与 UI）。
    pub const fn name(self) -> &'static str {
        match self {
            Self::Ascii => "ascii",
            Self::Utf16Le => "utf16le",
        }
    }
}

impl fmt::Display for StringEncoding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// 一条命中的字符串。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FoundString {
    /// 字符串在数据源中的起始字节偏移（UTF-16LE 指向低字节）。
    pub offset: u64,
    /// 解码后的文本。
    pub text: String,
    /// 编码。
    pub encoding: StringEncoding,
}

/// 字符串提取配置。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StringsCfg {
    /// 最短字符数，会被夹到至少 1。
    pub min_len: usize,
    /// 是否提取 ASCII。
    pub ascii: bool,
    /// 是否提取 UTF-16LE。
    pub utf16le: bool,
    /// 条数上限，达到后置 `truncated` 并停止收集。
    pub max_entries: usize,
}

impl Default for StringsCfg {
    fn default() -> Self {
        Self {
            min_len: DEFAULT_MIN_LEN,
            ascii: true,
            utf16le: true,
            max_entries: DEFAULT_MAX_ENTRIES,
        }
    }
}

impl StringsCfg {
    /// 规范化：`min_len >= 1`，`max_entries <= DEFAULT_MAX_ENTRIES`。
    pub fn normalized(mut self) -> Self {
        self.min_len = self.min_len.max(1);
        self.max_entries = self.max_entries.min(DEFAULT_MAX_ENTRIES);
        self
    }
}

/// 字符串提取结果。`truncated` 表示达到条数上限、可能还有更多命中。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StringsResult {
    /// 按发现顺序排列的字符串。
    pub entries: Vec<FoundString>,
    /// 是否因条数上限被截断。
    pub truncated: bool,
}

/// 判断字节是否可打印（ASCII 0x20~0x7E 或制表符）。
#[inline]
pub const fn is_printable(byte: u8) -> bool {
    matches!(byte, b'\t' | 0x20..=0x7E)
}

/// 从内存缓冲提取 ASCII 字符串。
pub fn extract_ascii(bytes: &[u8], min_len: usize) -> StringsResult {
    extract_strings(
        bytes,
        &StringsCfg {
            min_len,
            ascii: true,
            utf16le: false,
            max_entries: DEFAULT_MAX_ENTRIES,
        },
    )
}

/// 从内存缓冲提取 UTF-16LE 字符串。
pub fn extract_utf16le(bytes: &[u8], min_len: usize) -> StringsResult {
    extract_strings(
        bytes,
        &StringsCfg {
            min_len,
            ascii: false,
            utf16le: true,
            max_entries: DEFAULT_MAX_ENTRIES,
        },
    )
}

/// 按配置从内存缓冲提取字符串。
pub fn extract_strings(bytes: &[u8], cfg: &StringsCfg) -> StringsResult {
    let mut scanner = StringsScanner::new(*cfg);
    scanner.push(bytes, 0);
    scanner.finish()
}

/// 增量字符串扫描器：供单遍多分析器跨块复用。
#[derive(Debug)]
pub struct StringsScanner {
    cfg: StringsCfg,
    entries: Vec<FoundString>,
    truncated: bool,
    done: bool,
    ascii_start: Option<u64>,
    ascii_buf: Vec<u8>,
    utf16_start: [Option<u64>; 2],
    utf16_buf: [String; 2],
    utf16_pending: [Option<(u64, u8)>; 2],
}

impl StringsScanner {
    /// 按配置创建扫描器。
    pub fn new(cfg: StringsCfg) -> Self {
        let cfg = cfg.normalized();
        Self {
            cfg,
            entries: Vec::new(),
            truncated: false,
            done: false,
            ascii_start: None,
            ascii_buf: Vec::with_capacity(cfg.min_len.max(16)),
            utf16_start: [None; 2],
            utf16_buf: [String::new(), String::new()],
            utf16_pending: [None; 2],
        }
    }

    /// 喂入一块数据；`base` 是该块首字节的绝对偏移。
    pub fn push(&mut self, data: &[u8], base: u64) {
        if self.done {
            return;
        }
        if self.cfg.ascii {
            for (index, &byte) in data.iter().enumerate() {
                let offset = base.saturating_add(index as u64);
                if is_printable(byte) {
                    if self.ascii_start.is_none() {
                        self.ascii_start = Some(offset);
                    }
                    self.ascii_buf.push(byte);
                    if self.ascii_buf.len() >= MAX_STRING_CHARS {
                        self.flush_ascii();
                    }
                } else {
                    self.flush_ascii();
                }
                if self.done {
                    return;
                }
            }
        }
        if self.cfg.utf16le {
            for (index, &byte) in data.iter().enumerate() {
                let offset = base.saturating_add(index as u64);
                if offset % 2 == 0 {
                    // 偶数位置：先作为 high byte 补齐上一个奇数位置的低字节。
                    self.feed_utf16_high(1, byte);
                    self.utf16_pending[0] = Some((offset, byte));
                } else {
                    self.feed_utf16_high(0, byte);
                    self.utf16_pending[1] = Some((offset, byte));
                }
                if self.done {
                    return;
                }
            }
        }
    }

    /// 结束扫描，冲刷未闭合的 run，返回结果。
    pub fn finish(mut self) -> StringsResult {
        self.flush_ascii();
        self.flush_utf16(0);
        self.flush_utf16(1);
        StringsResult {
            entries: self.entries,
            truncated: self.truncated,
        }
    }

    fn feed_utf16_high(&mut self, phase: usize, high: u8) {
        let Some((start, low)) = self.utf16_pending[phase].take() else {
            return;
        };
        if is_printable(low) && high == 0 {
            if self.utf16_start[phase].is_none() {
                self.utf16_start[phase] = Some(start);
            }
            self.utf16_buf[phase].push(char::from(low));
            if self.utf16_buf[phase].len() >= MAX_STRING_CHARS {
                self.flush_utf16(phase);
            }
        } else {
            self.flush_utf16(phase);
        }
    }

    fn flush_ascii(&mut self) {
        let Some(start) = self.ascii_start.take() else {
            return;
        };
        let text = if self.ascii_buf.len() >= self.cfg.min_len {
            Some(String::from_utf8(std::mem::take(&mut self.ascii_buf)).unwrap_or_default())
        } else {
            self.ascii_buf.clear();
            None
        };
        if let Some(text) = text {
            self.emit(FoundString {
                offset: start,
                text,
                encoding: StringEncoding::Ascii,
            });
        }
    }

    fn flush_utf16(&mut self, phase: usize) {
        let Some(start) = self.utf16_start[phase].take() else {
            return;
        };
        let text = if self.utf16_buf[phase].chars().count() >= self.cfg.min_len {
            Some(std::mem::take(&mut self.utf16_buf[phase]))
        } else {
            self.utf16_buf[phase].clear();
            None
        };
        if let Some(text) = text {
            self.emit(FoundString {
                offset: start,
                text,
                encoding: StringEncoding::Utf16Le,
            });
        }
    }

    fn emit(&mut self, found: FoundString) {
        if self.entries.len() >= self.cfg.max_entries {
            self.truncated = true;
            self.done = true;
            return;
        }
        self.entries.push(found);
        // 只有再遇到下一条命中时才标记 truncated，避免“恰好 N 条”被误报。
        // 达到上限后仍继续扫描，直到下一条命中或输入结束。
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_extraction_respects_min_len_and_offsets() {
        let result = extract_ascii(b"\x00\x01hello\x00world!\x00abc", 4);
        assert_eq!(result.entries.len(), 2);
        assert_eq!(result.entries[0].offset, 2);
        assert_eq!(result.entries[0].text, "hello");
        assert_eq!(result.entries[1].offset, 8);
        assert_eq!(result.entries[1].text, "world!");
        assert!(!result.truncated);
    }

    #[test]
    fn ascii_tab_is_printable_but_control_is_not() {
        let result = extract_ascii(b"ab\tcd\nefgh", 4);
        assert_eq!(result.entries.len(), 2);
        assert_eq!(result.entries[0].text, "ab\tcd");
        assert_eq!(result.entries[1].text, "efgh");
    }

    #[test]
    fn utf16le_extraction_decodes_ascii_code_units() {
        let bytes = b"h\0e\0l\0l\0o\0\0\0w\0o\0r\0l\0d\0";
        let result = extract_utf16le(bytes, 4);
        let texts: Vec<&str> = result
            .entries
            .iter()
            .map(|entry| entry.text.as_str())
            .collect();
        assert!(texts.contains(&"hello"));
        assert!(texts.contains(&"world"));
        assert!(
            result
                .entries
                .iter()
                .all(|entry| entry.encoding == StringEncoding::Utf16Le)
        );
    }

    #[test]
    fn utf16le_catches_odd_alignment() {
        let mut bytes = vec![0xFF];
        bytes.extend_from_slice(b"t\0e\0s\0t\0");
        let result = extract_utf16le(&bytes, 4);
        assert_eq!(result.entries.len(), 1);
        assert_eq!(result.entries[0].offset, 1);
        assert_eq!(result.entries[0].text, "test");
    }

    #[test]
    fn exact_cap_is_not_reported_as_truncated() {
        let cfg = StringsCfg {
            min_len: 2,
            ascii: true,
            utf16le: false,
            max_entries: 2,
        };
        let result = extract_strings(b"aa\x00bb", &cfg);
        assert_eq!(result.entries.len(), 2);
        assert!(!result.truncated);
    }

    #[test]
    fn extraction_marks_truncated_at_cap() {
        let cfg = StringsCfg {
            min_len: 2,
            ascii: true,
            utf16le: false,
            max_entries: 2,
        };
        let result = extract_strings(b"aa\x00bb\x00cc\x00dd", &cfg);
        assert_eq!(result.entries.len(), 2);
        assert!(result.truncated);
    }

    #[test]
    fn scanner_handles_runs_across_chunks() {
        let mut scanner = StringsScanner::new(StringsCfg::default());
        scanner.push(b"\x00hel", 0);
        scanner.push(b"lo wo", 4);
        scanner.push(b"rld\x00", 9);
        let result = scanner.finish();
        assert_eq!(result.entries.len(), 1);
        assert_eq!(result.entries[0].offset, 1);
        assert_eq!(result.entries[0].text, "hello world");
    }

    #[test]
    fn scanner_handles_utf16_split_across_chunks() {
        let mut scanner = StringsScanner::new(StringsCfg {
            min_len: 4,
            ascii: false,
            utf16le: true,
            max_entries: 100,
        });
        scanner.push(b"h\0e", 0);
        scanner.push(b"\0l\0l\0o\0", 3);
        let result = scanner.finish();
        assert_eq!(result.entries.len(), 1);
        assert_eq!(result.entries[0].text, "hello");
    }
}
