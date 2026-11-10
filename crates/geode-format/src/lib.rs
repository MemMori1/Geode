//! Geode 自有二进制格式的编解码：`.geopatch` 与 `.geojournal`。
//!
//! DESIGN: 23 §3.6 / §3.8
//!
//! 本 crate 属于 L0（纯 CPU）：只认字节，不持有数据源，只依赖 [`geode_core`]。
//! 两个格式共用 §3.6 的记录帧
//! `kind u8 │ seq u64 │ payload_len u32 │ payload │ crc32 u32`，因此帧校验只有一份实现。
//! 所有多字节整数按**大端**读写；CRC-32 为 IEEE 802.3（reflected poly `0xEDB88320`，
//! init/xorout `0xFFFF_FFFF`）。
//!
//! 公开入口：
//!
//! - [`patch`]：32 B 定长头 + 记录帧序列，头与每帧各有一个 CRC-32；
//! - [`journal`]：80 B 定长头（头无 CRC，见 23 §3.8 的刻意设计）+ 记录帧序列。

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod crc32;
pub mod journal;
pub mod patch;

mod wire;

use core::fmt;

/// 格式层错误：**已登记的 `EB` 码** + 人类可读细节。
///
/// `code` 是判定与测试的唯一依据（`docs/errors.toml`），`detail` 只进日志/提示条，
/// 稳定文案由 UI 按码查表生成（25 §3.4）。当前可能出现的码：
/// `EB001`（版本过新）、`EB002`（magic 不符）、`EB005`（取值/范围违规）、
/// `EB011`（CRC 不符）、`EB012`（seq 不连续）、`EB013`（截断）、
/// `EB014`（长度不符）、`EB020`（超过单记录配额）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormatError {
    /// 已登记的错误码，例如 `EB002`。
    pub code: &'static str,
    /// 人类可读的细节；包含出错字节偏移与记录序号（适用时）。
    pub detail: String,
    /// 出错位置在输入字节中的偏移：头错误指向头起点，帧错误指向帧首。
    pub offset: Option<usize>,
    /// 出错的 **1-based** 记录号；头/尾级错误为 `None`。
    pub record: Option<u64>,
    /// journal 关卡 2/3 失败前已成功校验、可用于恢复前缀的记录数；其它情况为 0。
    pub recovered_records: usize,
}

impl FormatError {
    /// 用错误码与细节构造错误（偏移与记录号均未设置）。
    pub fn new(code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
            offset: None,
            record: None,
            recovered_records: 0,
        }
    }

    /// 附加出错字节偏移。
    pub fn at(mut self, offset: usize) -> Self {
        self.offset = Some(offset);
        self
    }

    /// 附加 **1-based** 记录号。
    pub fn in_record(mut self, record: u64) -> Self {
        self.record = Some(record);
        self
    }

    /// 附加“已成功校验的前缀记录数”（journal 的 Partial 语义）。
    pub fn with_recovered(mut self, records: usize) -> Self {
        self.recovered_records = records;
        self
    }
}

impl fmt::Display for FormatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.detail)?;
        if let Some(offset) = self.offset {
            write!(f, "（偏移 {offset:#X}）")?;
        }
        if let Some(record) = self.record {
            write!(f, "（第 {record} 条记录）")?;
        }
        Ok(())
    }
}

impl std::error::Error for FormatError {}
