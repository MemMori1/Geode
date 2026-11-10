//! `.geopatch` 补丁格式编解码。
//!
//! DESIGN: 23 §3.6 / §3.8（本模块实现 §3.6）
//!
//! 布局：32 B 定长头 + 记录帧序列；尾部允许 1..=4 个 `0x00` 填充（读侧忽略）。
//! 头 CRC 覆盖 `0..12`，每帧 CRC 覆盖 `0..13+payload_len`。所有整数大端。

use crate::FormatError;
use crate::wire::{
    DEFAULT_MAX_RECORD_BYTES, FRAME_HEADER_LEN, len_u16, put_u16, put_u32, put_u64, read_frame,
    read_u16, read_u32, read_u64, write_frame,
};
use geode_core::Offset;

/// 文件 magic：`"GEOPATCH"`。
pub const MAGIC: &[u8; 8] = b"GEOPATCH";

/// 定长头长度（字节）。
pub const HEADER_LEN: usize = 32;

/// `flags` 的 bit0：置位时第 1 条记录必须是 [`RecordBody::FileMeta`]。
pub const FLAG_HAS_FILE_META: u16 = 0x0001;

/// 解码后的 `.geopatch` 逻辑结构。
///
/// `record_count` 不单独保存：它必须等于 [`File::records`] 的长度（23 §3.6 的 EB014 约束），
/// 由 [`encode`] 自动写入。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct File {
    /// 格式版本；当前恒为 `1`。
    pub fmt_ver: u16,
    /// 头标志；bit0 为 [`FLAG_HAS_FILE_META`]，bit1..15 必须为 0。
    pub flags: u16,
    /// 应用前目标字节数，用于预检。
    pub base_size: u64,
    /// 记录序列（含未知 kind 的保留记录，保证可字节级往返）。
    pub records: Vec<Record>,
}

impl File {
    /// 构造一个空的 v1 补丁文件。
    pub fn new(base_size: u64) -> Self {
        Self {
            fmt_ver: 1,
            flags: 0,
            base_size,
            records: Vec::new(),
        }
    }
}

impl Default for File {
    fn default() -> Self {
        Self::new(0)
    }
}

/// 一条记录：序号 + 记录体。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    /// 序号，从 1 起严格 +1（23 §3.6 的 EB012 约束）。
    pub seq: u64,
    /// 记录体。
    pub body: RecordBody,
}

impl Record {
    /// 用序号与记录体构造一条记录。
    pub fn new(seq: u64, body: RecordBody) -> Self {
        Self { seq, body }
    }
}

/// `.geopatch` 的记录体。
///
/// 未知 `kind` 在读侧必须跳过（EB006 告警）。为支持“解码再编码字节完全一致”，
/// 这里用 [`RecordBody::Unknown`] 原样保留其载荷。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecordBody {
    /// `0x01` 等长替换：把 `at` 处的 `data.len()` 字节替换为 `data`。
    Overwrite {
        /// 目标偏移。
        at: Offset,
        /// 新数据；长度即被替换长度。
        data: Vec<u8>,
    },
    /// `0x02` 插入：在 `at` 前插入 `data`。
    Insert {
        /// 目标偏移。
        at: Offset,
        /// 插入的数据。
        data: Vec<u8>,
    },
    /// `0x03` 删除 `[at, at + len)`。
    Remove {
        /// 目标偏移。
        at: Offset,
        /// 删除长度。
        len: u64,
    },
    /// `0x04` 撤销分组边界。
    Checkpoint {
        /// 分组标签（UTF-8）。
        label: String,
    },
    /// `0x12` UTF-8 注释，≤4 KiB，无副作用。
    Comment {
        /// 注释文本。
        text: String,
    },
    /// `0x10` 建议性目标标识；`path` **不得**用于自动定位。
    FileMeta {
        /// 头部采样长度。
        head_len: u32,
        /// base 头部 BLAKE3（32 B 原始摘要）。
        base_head_blake3: [u8; 32],
        /// 建议性路径（UTF-8）。
        path: String,
    },
    /// `0x11` 应用后长度；必须是最后一条记录。
    FinalLen {
        /// 应用后的目标字节数。
        final_size: u64,
    },
    /// 未知 kind：读侧跳过语义，但保留原始载荷以保证往返。
    Unknown {
        /// 原始 `kind` 字节。
        kind: u8,
        /// 原始载荷。
        payload: Vec<u8>,
    },
}

impl RecordBody {
    /// 该记录体在盘上的 `kind` 字节。
    pub fn kind(&self) -> u8 {
        match self {
            Self::Overwrite { .. } => 0x01,
            Self::Insert { .. } => 0x02,
            Self::Remove { .. } => 0x03,
            Self::Checkpoint { .. } => 0x04,
            Self::Comment { .. } => 0x12,
            Self::FileMeta { .. } => 0x10,
            Self::FinalLen { .. } => 0x11,
            Self::Unknown { kind, .. } => *kind,
        }
    }

    /// 构造线上的 payload（定长 BE + 裸字节）。
    fn payload(&self) -> Vec<u8> {
        let mut out = Vec::new();
        match self {
            Self::Overwrite { at, data } | Self::Insert { at, data } => {
                put_u64(&mut out, at.get());
                put_u64(&mut out, data.len() as u64);
                out.extend_from_slice(data);
            }
            Self::Remove { at, len } => {
                put_u64(&mut out, at.get());
                put_u64(&mut out, *len);
            }
            Self::Checkpoint { label } => {
                put_u16(&mut out, len_u16(label.len(), "Checkpoint label"));
                out.extend_from_slice(label.as_bytes());
            }
            Self::Comment { text } => {
                put_u16(&mut out, len_u16(text.len(), "Comment text"));
                out.extend_from_slice(text.as_bytes());
            }
            Self::FileMeta {
                head_len,
                base_head_blake3,
                path,
            } => {
                put_u32(&mut out, *head_len);
                out.extend_from_slice(base_head_blake3);
                put_u16(&mut out, len_u16(path.len(), "FileMeta path"));
                out.extend_from_slice(path.as_bytes());
            }
            Self::FinalLen { final_size } => put_u64(&mut out, *final_size),
            Self::Unknown { payload, .. } => out.extend_from_slice(payload),
        }
        out
    }
}

/// 把逻辑结构编码为 `.geopatch` 字节。
///
/// # Panics
///
/// 当记录数超过 `u32::MAX`、单条载荷超过 `u32::MAX`，或 label/path 超过 `u16::MAX`
/// 字节时 panic——这些值在 `File` 构造时未被限制，但不可能出现在合法文件中。
pub fn encode(file: &File) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_LEN + file.records.len() * 32);
    out.extend_from_slice(MAGIC);
    put_u16(&mut out, file.fmt_ver);
    put_u16(&mut out, file.flags);
    let header_crc = crate::crc32::crc32(&out[..12]);
    put_u32(&mut out, header_crc);
    put_u64(&mut out, file.base_size);
    let record_count = u32::try_from(file.records.len()).expect("记录数必须能被 u32 表示");
    put_u32(&mut out, record_count);
    put_u32(&mut out, 0); // reserved：写侧恒 0

    for record in &file.records {
        write_frame(
            &mut out,
            record.body.kind(),
            record.seq,
            &record.body.payload(),
        );
    }
    out
}

/// 解码并**全量校验** `.geopatch` 字节。
///
/// 按 23 §3.6 的应用规则先只扫不应用：magic、版本、flags、头 CRC、逐帧 CRC、
/// `seq` 连续性、`record_count`、`FinalLen` 位置。任何一项失败都返回带 `EB` 码的
/// [`FormatError`]，调用方不得把部分结果当作可应用补丁。
pub fn decode(bytes: &[u8]) -> Result<File, FormatError> {
    if bytes.len() < MAGIC.len() {
        return Err(FormatError::new(
            "EB013",
            format!(
                ".geopatch 头截断：需要至少 {} B，实际 {} B",
                MAGIC.len(),
                bytes.len()
            ),
        )
        .at(0));
    }
    if &bytes[..MAGIC.len()] != MAGIC {
        return Err(FormatError::new(
            "EB002",
            format!(
                ".geopatch magic 不符：期望 GEOPATCH，实际 {}",
                hex(&bytes[..MAGIC.len()])
            ),
        )
        .at(0));
    }
    if bytes.len() < HEADER_LEN {
        return Err(FormatError::new(
            "EB013",
            format!(
                ".geopatch 头截断：需要 {HEADER_LEN} B，实际 {} B",
                bytes.len()
            ),
        )
        .at(0));
    }

    let fmt_ver = read_u16(bytes, 8);
    if fmt_ver != 1 {
        return Err(FormatError::new(
            "EB001",
            format!(".geopatch 版本过新：fmt_ver={fmt_ver}，本实现只支持 1"),
        )
        .at(8));
    }
    let stored_header_crc = read_u32(bytes, 12);
    let actual_header_crc = crate::crc32::crc32(&bytes[..12]);
    if stored_header_crc != actual_header_crc {
        return Err(FormatError::new(
            "EB011",
            format!(
                ".geopatch 头 CRC 不符：存储 {stored_header_crc:#010X}，实际 {actual_header_crc:#010X}"
            ),
        )
        .at(12));
    }
    let flags = read_u16(bytes, 10);
    if flags & !FLAG_HAS_FILE_META != 0 {
        return Err(FormatError::new(
            "EB005",
            format!(".geopatch flags 含未定义位：{flags:#06X}（只允许 bit0 HAS_FILE_META）"),
        )
        .at(10));
    }

    let base_size = read_u64(bytes, 16);
    let record_count = read_u32(bytes, 24) as usize;

    let mut records = Vec::new();
    let mut at = HEADER_LEN;
    while at < bytes.len() {
        if is_trailing_padding(bytes, at) {
            break;
        }
        let frame = read_frame(bytes, at, ".geopatch", DEFAULT_MAX_RECORD_BYTES)?;
        let expected_seq = records.len() as u64 + 1;
        if frame.seq != expected_seq {
            return Err(FormatError::new(
                "EB012",
                format!(
                    ".geopatch seq 不连续：第 {} 条记录期望 {expected_seq}，实际 {}",
                    records.len() + 1,
                    frame.seq
                ),
            )
            .at(at)
            .in_record(records.len() as u64 + 1));
        }
        let body = parse_record(frame.kind, frame.payload).map_err(|error| {
            error
                .at(at)
                .in_record(records.len() as u64 + 1)
                .with_recovered(records.len())
        })?;
        records.push(Record {
            seq: frame.seq,
            body,
        });
        at = frame.next;
    }

    if records.len() != record_count {
        return Err(FormatError::new(
            "EB014",
            format!(
                ".geopatch record_count 不符：头声明 {record_count} 条，实际解析 {} 条",
                records.len()
            ),
        )
        .at(24));
    }

    if flags & FLAG_HAS_FILE_META != 0 && !records.first().is_some_and(|r| r.body.kind() == 0x10) {
        return Err(FormatError::new(
            "EB014",
            ".geopatch flags 置位 HAS_FILE_META，但第 1 条记录不是 FileMeta（0x10）",
        )
        .at(HEADER_LEN));
    }

    if let Some(index) = records
        .iter()
        .position(|r| matches!(r.body, RecordBody::FinalLen { .. }))
    {
        if index + 1 != records.len() {
            return Err(FormatError::new(
                "EB014",
                format!(
                    ".geopatch FinalLen 必须是最后一条记录，实际位于第 {} 条（共 {} 条）",
                    index + 1,
                    records.len()
                ),
            )
            .in_record(index as u64 + 1));
        }
    }

    Ok(File {
        fmt_ver,
        flags,
        base_size,
        records,
    })
}

/// 判断 `at` 起是否为允许的 1..=4 个 `0x00` 尾部填充。
fn is_trailing_padding(bytes: &[u8], at: usize) -> bool {
    let tail = &bytes[at..];
    !tail.is_empty() && tail.len() <= 4 && tail.iter().all(|&byte| byte == 0)
}

/// 按 `kind` 解释一帧载荷。
fn parse_record(kind: u8, payload: &[u8]) -> Result<RecordBody, FormatError> {
    match kind {
        0x01 | 0x02 => {
            if payload.len() < 16 {
                return Err(FormatError::new(
                    "EB014",
                    format!(
                        "Overwrite/Insert 载荷至少 16 B（at + len），实际 {} B",
                        payload.len()
                    ),
                ));
            }
            let at = Offset::new(read_u64(payload, 0));
            let declared = read_u64(payload, 8);
            let data = &payload[16..];
            if declared != data.len() as u64 {
                return Err(FormatError::new(
                    "EB014",
                    format!(
                        "Overwrite/Insert 声明的 len={declared} 与载荷数据 {} B 不符",
                        data.len()
                    ),
                ));
            }
            let data = data.to_vec();
            Ok(if kind == 0x01 {
                RecordBody::Overwrite { at, data }
            } else {
                RecordBody::Insert { at, data }
            })
        }
        0x03 => {
            if payload.len() != 16 {
                return Err(FormatError::new(
                    "EB014",
                    format!("Remove 载荷必须恰好 16 B，实际 {} B", payload.len()),
                ));
            }
            Ok(RecordBody::Remove {
                at: Offset::new(read_u64(payload, 0)),
                len: read_u64(payload, 8),
            })
        }
        0x04 => parse_label(payload).map(|label| RecordBody::Checkpoint { label }),
        0x12 => {
            let text = parse_utf8_string(payload, "Comment text")?;
            if text.len() > 4096 {
                return Err(FormatError::new(
                    "EB005",
                    format!("Comment 文本 {} B 超过 4 KiB 上限", text.len()),
                ));
            }
            Ok(RecordBody::Comment { text })
        }
        0x10 => {
            if payload.len() < 38 {
                return Err(FormatError::new(
                    "EB014",
                    format!(
                        "FileMeta 载荷至少 38 B（head_len + blake3 + path_len），实际 {} B",
                        payload.len()
                    ),
                ));
            }
            let head_len = read_u32(payload, 0);
            let mut base_head_blake3 = [0u8; 32];
            base_head_blake3.copy_from_slice(&payload[4..36]);
            let path_len = read_u16(payload, 36) as usize;
            let expected = 38 + path_len;
            if payload.len() != expected {
                return Err(FormatError::new(
                    "EB014",
                    format!(
                        "FileMeta 载荷长度不符：声明 {expected} B，实际 {} B",
                        payload.len()
                    ),
                ));
            }
            let path = std::str::from_utf8(&payload[38..]).map_err(|error| {
                FormatError::new("EB005", format!("FileMeta path 不是合法 UTF-8：{error}"))
            })?;
            Ok(RecordBody::FileMeta {
                head_len,
                base_head_blake3,
                path: path.to_owned(),
            })
        }
        0x11 => {
            if payload.len() != 8 {
                return Err(FormatError::new(
                    "EB014",
                    format!("FinalLen 载荷必须恰好 8 B，实际 {} B", payload.len()),
                ));
            }
            Ok(RecordBody::FinalLen {
                final_size: read_u64(payload, 0),
            })
        }
        other => Ok(RecordBody::Unknown {
            kind: other,
            payload: payload.to_vec(),
        }),
    }
}

/// 解析 `label_len u16 │ label`。
fn parse_label(payload: &[u8]) -> Result<String, FormatError> {
    parse_utf8_string(payload, "Checkpoint label")
}

/// 解析 `len u16 │ bytes` 并校验 UTF-8。
fn parse_utf8_string(payload: &[u8], what: &str) -> Result<String, FormatError> {
    if payload.len() < 2 {
        return Err(FormatError::new(
            "EB014",
            format!("{what} 载荷至少 2 B（长度前缀），实际 {} B", payload.len()),
        ));
    }
    let declared = read_u16(payload, 0) as usize;
    let expected = 2 + declared;
    if payload.len() != expected {
        return Err(FormatError::new(
            "EB014",
            format!(
                "{what} 载荷长度不符：声明 {declared} B 文本（共 {expected} B），实际 {} B",
                payload.len()
            ),
        ));
    }
    std::str::from_utf8(&payload[2..])
        .map(str::to_owned)
        .map_err(|error| FormatError::new("EB005", format!("{what} 不是合法 UTF-8：{error}")))
}

/// 把字节串格式化为大写 hex（仅用于错误细节）。
fn hex(bytes: &[u8]) -> String {
    use core::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02X}");
    }
    out
}

/// 帧头长度在本模块内也用于错误定位。
const _: usize = FRAME_HEADER_LEN;

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> File {
        let mut file = File::new(4096);
        file.flags = FLAG_HAS_FILE_META;
        file.records.push(Record::new(
            1,
            RecordBody::FileMeta {
                head_len: 65536,
                base_head_blake3: [0xAB; 32],
                path: "a.bin".to_owned(),
            },
        ));
        file.records.push(Record::new(
            2,
            RecordBody::Overwrite {
                at: Offset::new(0x10),
                data: vec![1, 2, 3, 4],
            },
        ));
        file.records.push(Record::new(
            3,
            RecordBody::Insert {
                at: Offset::new(0x20),
                data: vec![5, 6],
            },
        ));
        file.records.push(Record::new(
            4,
            RecordBody::Remove {
                at: Offset::new(0x30),
                len: 8,
            },
        ));
        file.records.push(Record::new(
            5,
            RecordBody::Checkpoint {
                label: "paste".to_owned(),
            },
        ));
        file.records.push(Record::new(
            6,
            RecordBody::Comment {
                text: "测试注释".to_owned(),
            },
        ));
        file.records.push(Record::new(
            7,
            RecordBody::Unknown {
                kind: 0x7F,
                payload: vec![9, 9],
            },
        ));
        file.records
            .push(Record::new(8, RecordBody::FinalLen { final_size: 4100 }));
        file
    }

    #[test]
    fn round_trip_all_record_kinds() {
        let file = sample();
        let bytes = encode(&file);
        assert_eq!(decode(&bytes).unwrap(), file);
    }

    #[test]
    fn header_is_32_bytes_and_crc_covers_first_12() {
        let bytes = encode(&sample());
        assert_eq!(read_u32(&bytes, 12), crate::crc32::crc32(&bytes[..12]));
        assert_eq!(bytes.len(), HEADER_LEN + bytes[HEADER_LEN..].len());
    }

    #[test]
    fn trailing_zero_padding_is_ignored() {
        let mut bytes = encode(&sample());
        bytes.extend_from_slice(&[0, 0, 0]);
        assert_eq!(decode(&bytes).unwrap(), sample());
    }

    #[test]
    fn bad_magic_is_eb002() {
        let mut bytes = encode(&sample());
        bytes[7] = b'X';
        assert_eq!(decode(&bytes).unwrap_err().code, "EB002");
    }

    #[test]
    fn bad_header_crc_is_eb011() {
        let mut bytes = encode(&sample());
        bytes[12..16].copy_from_slice(&0xDEAD_BEEFu32.to_be_bytes());
        assert_eq!(decode(&bytes).unwrap_err().code, "EB011");
    }

    #[test]
    fn bad_frame_crc_is_eb011() {
        let mut bytes = encode(&File {
            records: vec![Record::new(1, RecordBody::FinalLen { final_size: 0 })],
            ..File::new(0)
        });
        let last = bytes.len() - 1;
        bytes[last] ^= 0xFF;
        assert_eq!(decode(&bytes).unwrap_err().code, "EB011");
    }

    #[test]
    fn seq_gap_is_eb012() {
        let mut file = File::new(2);
        file.records.push(Record::new(
            1,
            RecordBody::Remove {
                at: Offset::ZERO,
                len: 1,
            },
        ));
        file.records.push(Record::new(
            3,
            RecordBody::Remove {
                at: Offset::new(1),
                len: 1,
            },
        ));
        assert_eq!(decode(&encode(&file)).unwrap_err().code, "EB012");
    }

    #[test]
    fn truncated_header_and_frame_are_eb013() {
        assert_eq!(decode(b"GEO").unwrap_err().code, "EB013");
        let bytes = encode(&sample());
        assert_eq!(decode(&bytes[..20]).unwrap_err().code, "EB013");
        assert_eq!(decode(&bytes[..bytes.len() - 2]).unwrap_err().code, "EB013");
    }

    #[test]
    fn record_count_mismatch_is_eb014() {
        let mut bytes = encode(&sample());
        bytes[24..28].copy_from_slice(&99u32.to_be_bytes());
        assert_eq!(decode(&bytes).unwrap_err().code, "EB014");
    }

    #[test]
    fn final_len_must_be_last() {
        let mut file = File::new(0);
        file.records
            .push(Record::new(1, RecordBody::FinalLen { final_size: 0 }));
        file.records.push(Record::new(
            2,
            RecordBody::Remove {
                at: Offset::ZERO,
                len: 1,
            },
        ));
        assert_eq!(decode(&encode(&file)).unwrap_err().code, "EB014");
    }

    #[test]
    fn file_meta_flag_requires_first_record() {
        let mut file = File::new(0);
        file.flags = FLAG_HAS_FILE_META;
        file.records
            .push(Record::new(1, RecordBody::FinalLen { final_size: 0 }));
        assert_eq!(decode(&encode(&file)).unwrap_err().code, "EB014");
    }

    #[test]
    fn unknown_flags_bits_are_eb005() {
        let mut file = File::new(0);
        file.flags = 0x0002;
        assert_eq!(decode(&encode(&file)).unwrap_err().code, "EB005");
    }

    #[test]
    fn too_new_version_is_eb001() {
        let mut file = File::new(0);
        file.fmt_ver = 2;
        assert_eq!(decode(&encode(&file)).unwrap_err().code, "EB001");
    }

    #[test]
    fn overwrite_declared_len_mismatch_is_eb014() {
        let mut file = File::new(0);
        file.records.push(Record::new(
            1,
            RecordBody::Overwrite {
                at: Offset::ZERO,
                data: vec![1, 2],
            },
        ));
        let mut bytes = encode(&file);
        // 把 payload 里的 len 改成 3（帧 CRC 同步重算，避免先撞 EB011）。
        let payload_len = read_u32(&bytes, HEADER_LEN + 9) as usize;
        let payload_at = HEADER_LEN + FRAME_HEADER_LEN;
        assert_eq!(payload_len, 18);
        bytes[payload_at + 8..payload_at + 16].copy_from_slice(&3u64.to_be_bytes());
        let crc_at = payload_at + payload_len;
        let crc = crate::crc32::crc32(&bytes[HEADER_LEN..crc_at]);
        bytes[crc_at..crc_at + 4].copy_from_slice(&crc.to_be_bytes());
        assert_eq!(decode(&bytes).unwrap_err().code, "EB014");
    }
}
