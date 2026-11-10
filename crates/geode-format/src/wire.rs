//! 两种二进制格式共用的记录帧读写。
//!
//! DESIGN: 23 §3.6 / §3.8
//!
//! 帧布局固定为 `kind u8 │ seq u64 │ payload_len u32 │ payload │ crc32 u32`，
//! CRC 覆盖帧首到 payload 末尾（`0..13+payload_len`，不含 CRC 自身）。

use crate::FormatError;
use crate::crc32::crc32;

/// 帧头长度（`kind` + `seq` + `payload_len`）。
pub(crate) const FRAME_HEADER_LEN: usize = 13;

/// 单帧固定开销（帧头 + CRC）。
pub(crate) const FRAME_OVERHEAD: usize = FRAME_HEADER_LEN + 4;

/// `formats.geopatch.max_record_bytes` 的默认值：64 MiB（23 §8）。
pub(crate) const DEFAULT_MAX_RECORD_BYTES: usize = 64 * 1024 * 1024;

/// 解析出的原始帧（payload 仍为未解释字节）。
pub(crate) struct RawFrame<'a> {
    /// 记录类型。
    pub kind: u8,
    /// 记录序号。
    pub seq: u64,
    /// 载荷（`payload_len` 字节）。
    pub payload: &'a [u8],
    /// 下一帧的起始偏移。
    pub next: usize,
}

/// 从 `at` 处读取并校验一帧。
///
/// 判定顺序与 23 §3.6 的应用规则一致：先看长度是否足够（EB013），再看
/// `payload_len` 是否超配额（EB020），最后校验 CRC（EB011）。`seq` 由调用方检查，
/// 因为“期望值”依赖已解析的记录数。
pub(crate) fn read_frame<'a>(
    bytes: &'a [u8],
    at: usize,
    format: &str,
    max_record_bytes: usize,
) -> Result<RawFrame<'a>, FormatError> {
    let remaining = bytes.len().saturating_sub(at);
    if remaining < FRAME_HEADER_LEN {
        return Err(FormatError::new(
            "EB013",
            format!(
                "{format} 记录帧头截断：需要 {FRAME_HEADER_LEN} B，偏移 {at:#X} 处仅剩 {remaining} B"
            ),
        )
        .at(at));
    }

    let kind = bytes[at];
    let seq = read_u64(bytes, at + 1);
    let payload_len = read_u32(bytes, at + 9) as usize;

    if payload_len > max_record_bytes {
        return Err(FormatError::new(
            "EB020",
            format!(
                "{format} 记录载荷 {payload_len} B 超过上限 {max_record_bytes} B \
                 （formats.geopatch.max_record_bytes）"
            ),
        )
        .at(at));
    }

    let total = FRAME_OVERHEAD + payload_len;
    if total > remaining {
        let missing = total - remaining;
        return Err(FormatError::new(
            "EB013",
            format!(
                "{format} 记录帧截断：需要 {total} B，偏移 {at:#X} 处仅剩 {remaining} B，缺 {missing} B"
            ),
        )
        .at(at));
    }

    let payload_end = at + FRAME_HEADER_LEN + payload_len;
    let stored = read_u32(bytes, payload_end);
    let actual = crc32(&bytes[at..payload_end]);
    if stored != actual {
        return Err(FormatError::new(
            "EB011",
            format!("{format} 记录 CRC 不符：存储 {stored:#010X}，实际 {actual:#010X}"),
        )
        .at(at));
    }

    Ok(RawFrame {
        kind,
        seq,
        payload: &bytes[at + FRAME_HEADER_LEN..payload_end],
        next: at + total,
    })
}

/// 写出一帧（含 CRC）。
pub(crate) fn write_frame(out: &mut Vec<u8>, kind: u8, seq: u64, payload: &[u8]) {
    let payload_len = u32::try_from(payload.len()).expect("记录载荷必须能被 u32 表示");
    let start = out.len();
    out.push(kind);
    out.extend_from_slice(&seq.to_be_bytes());
    out.extend_from_slice(&payload_len.to_be_bytes());
    out.extend_from_slice(payload);
    let crc = crc32(&out[start..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

/// 读大端 `u16`；调用方保证 `at + 2 <= bytes.len()`。
pub(crate) fn read_u16(bytes: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([bytes[at], bytes[at + 1]])
}

/// 读大端 `u32`；调用方保证 `at + 4 <= bytes.len()`。
pub(crate) fn read_u32(bytes: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

/// 读大端 `u64`；调用方保证 `at + 8 <= bytes.len()`。
pub(crate) fn read_u64(bytes: &[u8], at: usize) -> u64 {
    u64::from_be_bytes([
        bytes[at],
        bytes[at + 1],
        bytes[at + 2],
        bytes[at + 3],
        bytes[at + 4],
        bytes[at + 5],
        bytes[at + 6],
        bytes[at + 7],
    ])
}

/// 写大端 `u16`。
pub(crate) fn put_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_be_bytes());
}

/// 写大端 `u32`。
pub(crate) fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_be_bytes());
}

/// 写大端 `u64`。
pub(crate) fn put_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_be_bytes());
}

/// 把 `usize` 转成协议要求的 `u16` 长度；超限时给出明确 panic 信息。
pub(crate) fn len_u16(len: usize, what: &str) -> u16 {
    u16::try_from(len).unwrap_or_else(|_| panic!("{what} 长度 {len} B 超过 u16 上限"))
}
