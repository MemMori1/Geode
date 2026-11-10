//! `.geojournal` 编辑日志编解码。
//!
//! DESIGN: 23 §3.6 / §3.8（本模块实现 §3.8）
//!
//! 布局：80 B 定长头 + 记录帧序列（无尾部填充）。头无 CRC 是刻意的；每帧 CRC
//! 覆盖 `0..13+payload_len`，所有整数大端。回放四道关卡见 23 §3.8。

use crate::FormatError;
use crate::wire::{
    DEFAULT_MAX_RECORD_BYTES, FRAME_HEADER_LEN, len_u16, put_u16, put_u32, put_u64, read_frame,
    read_u16, read_u32, read_u64, write_frame,
};
use geode_core::Offset;

/// 文件 magic：`"GEOJRN01"`。
pub const MAGIC: &[u8; 8] = b"GEOJRN01";

/// 定长头长度（字节）。
pub const HEADER_LEN: usize = 80;

/// 解码后的 `.geojournal` 逻辑结构。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct File {
    /// 格式版本；当前恒为 `1`。
    pub fmt_ver: u16,
    /// 保留头字段；规范要求写 0，解码保留原值以便字节级往返。
    pub flags: u16,
    /// 文档 ULID 的 16 B 二进制形式。
    pub doc_uuid: [u8; 16],
    /// base 来源类型。
    pub base_kind: BaseKind,
    /// base 字节数（关卡 4）。
    pub base_size: u64,
    /// 头部采样长度。
    pub head_len: u32,
    /// base 头部 BLAKE3（32 B 原始摘要，关卡 4 的唯一依据）。
    pub base_head_blake3: [u8; 32],
    /// 记录序列（含未知 kind 的保留记录，保证可字节级往返）。
    pub records: Vec<Record>,
    /// 是否只恢复出 CRC/序号异常之前的前缀（关卡 2/3）。
    ///
    /// 该字段是解码结果，不参与编码；完整文件的 [`decode`] 恒为 `false`。
    pub partial: bool,
}

impl File {
    /// 构造一个空的 v1 日志。
    pub fn new(
        doc_uuid: [u8; 16],
        base_kind: BaseKind,
        base_size: u64,
        head_len: u32,
        base_head_blake3: [u8; 32],
    ) -> Self {
        Self {
            fmt_ver: 1,
            flags: 0,
            doc_uuid,
            base_kind,
            base_size,
            head_len,
            base_head_blake3,
            records: Vec::new(),
            partial: false,
        }
    }
}

impl Default for File {
    fn default() -> Self {
        Self::new([0; 16], BaseKind::File, 0, 4096, [0; 32])
    }
}

/// `.geojournal` 头的 `base_kind`。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BaseKind {
    /// `0x00` 磁盘文件。
    File,
    /// `0x01` 未命名文档。
    Untitled,
    /// `0x02` provider 提供的数据源。
    Provider,
}

impl BaseKind {
    /// 该类型在盘上的字节值。
    pub fn code(self) -> u8 {
        match self {
            Self::File => 0x00,
            Self::Untitled => 0x01,
            Self::Provider => 0x02,
        }
    }

    /// 从盘上字节值解析；未知值按 EB005 拒绝。
    fn from_byte(byte: u8) -> Result<Self, FormatError> {
        match byte {
            0x00 => Ok(Self::File),
            0x01 => Ok(Self::Untitled),
            0x02 => Ok(Self::Provider),
            other => Err(FormatError::new(
                "EB005",
                format!(".geojournal base_kind 未知：{other:#04X}（只允许 0x00/0x01/0x02）"),
            )
            .at(28)),
        }
    }
}

/// 一条记录：序号 + 记录体。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    /// 序号，从 1 起严格 +1（关卡 3，EB012）。
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

/// `.geojournal` 的记录体。
///
/// 日志只记录 `EditOp`，绝不记录文件副本。未知 `kind` 读侧跳过（EB006），但为字节级
/// 往返保留为 [`RecordBody::Unknown`]。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecordBody {
    /// `0x01` 等长替换。
    Overwrite {
        /// 目标偏移。
        at: Offset,
        /// 新数据；长度即被替换长度。
        data: Vec<u8>,
    },
    /// `0x02` 插入。
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
    /// `0x05` 保存成功后截断重写日志并更新头 `base_*`。
    SaveMark {
        /// 新 base 字节数。
        new_base_size: u64,
        /// 新 base 头部 BLAKE3（32 B 原始摘要）。
        new_head_blake3: [u8; 32],
    },
    /// `0x06` 回放后的长度；协议要求位于末条。
    FinalLen {
        /// 回放后的字节数。
        len: u64,
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
            Self::SaveMark { .. } => 0x05,
            Self::FinalLen { .. } => 0x06,
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
            Self::SaveMark {
                new_base_size,
                new_head_blake3,
            } => {
                put_u64(&mut out, *new_base_size);
                out.extend_from_slice(new_head_blake3);
            }
            Self::FinalLen { len } => put_u64(&mut out, *len),
            Self::Unknown { payload, .. } => out.extend_from_slice(payload),
        }
        out
    }
}

/// 部分解码结果：可回放前缀 + 首个异常。
#[derive(Clone, Debug)]
pub struct Decoded {
    /// 已成功解析的头 + 记录前缀；`file.partial` 与 `error.is_some()` 一致。
    pub file: File,
    /// 首个异常；`None` 表示整份文件完整有效。
    pub error: Option<FormatError>,
}

impl Decoded {
    /// 是否只恢复了前缀（关卡 2/3 等）。
    pub fn is_partial(&self) -> bool {
        self.error.is_some()
    }
}

/// 把逻辑结构编码为 `.geojournal` 字节。
///
/// # Panics
///
/// 当单条载荷超过 `u32::MAX`，或 Checkpoint label 超过 `u16::MAX` 字节时 panic——
/// 这类值不可能出现在合法文件中。
pub fn encode(file: &File) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_LEN + file.records.len() * 32);
    out.extend_from_slice(MAGIC);
    put_u16(&mut out, file.fmt_ver);
    put_u16(&mut out, file.flags);
    out.extend_from_slice(&file.doc_uuid);
    out.push(file.base_kind.code());
    put_u64(&mut out, file.base_size);
    put_u32(&mut out, file.head_len);
    out.extend_from_slice(&file.base_head_blake3);
    out.extend_from_slice(&[0u8; 7]); // reserved：写侧恒 0

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

/// 解码并严格校验 `.geojournal` 字节。
///
/// 任一关卡失败（magic、版本、头截断、帧 CRC、序号、载荷长度、末条 FinalLen 位置）都
/// 返回 `Err`。若需要“CRC 坏但已回放前缀仍可恢复”的语义，用 [`decode_prefix`]。
pub fn decode(bytes: &[u8]) -> Result<File, FormatError> {
    let decoded = decode_prefix(bytes)?;
    match decoded.error {
        Some(error) => Err(error),
        None => Ok(decoded.file),
    }
}

/// 前缀解码：返回已成功解析的头与记录，以及首个异常（若有）。
///
/// - 头本身不可识别（magic/版本/头截断/base_kind 非法）⇒ `Err`，没有可恢复前缀；
/// - 帧 CRC 坏 ⇒ `Ok(Decoded { error: Some(EB011) })`，前缀仍可用于恢复（关卡 2，
///   `Partial`）；
/// - 序号断裂 ⇒ `Ok(Decoded { error: Some(EB012) })`，按首个异常前截断（关卡 3）；
/// - 帧截断 ⇒ `Ok(Decoded { error: Some(EB013) })`。
pub fn decode_prefix(bytes: &[u8]) -> Result<Decoded, FormatError> {
    let header = parse_header(bytes)?;

    let mut records = Vec::new();
    let mut at = HEADER_LEN;
    let mut failure = None;

    while at < bytes.len() {
        let frame = match read_frame(bytes, at, ".geojournal", DEFAULT_MAX_RECORD_BYTES) {
            Ok(frame) => frame,
            Err(error) => {
                failure = Some(
                    error
                        .at(at)
                        .in_record(records.len() as u64 + 1)
                        .with_recovered(records.len()),
                );
                break;
            }
        };
        let expected_seq = records.len() as u64 + 1;
        if frame.seq != expected_seq {
            failure = Some(
                FormatError::new(
                    "EB012",
                    format!(
                        ".geojournal seq 不连续：第 {} 条记录期望 {expected_seq}，实际 {}",
                        records.len() + 1,
                        frame.seq
                    ),
                )
                .at(at)
                .in_record(records.len() as u64 + 1)
                .with_recovered(records.len()),
            );
            break;
        }
        let body = match parse_record(frame.kind, frame.payload) {
            Ok(body) => body,
            Err(error) => {
                failure = Some(
                    error
                        .at(at)
                        .in_record(records.len() as u64 + 1)
                        .with_recovered(records.len()),
                );
                break;
            }
        };
        records.push(Record {
            seq: frame.seq,
            body,
        });
        at = frame.next;
    }

    if failure.is_none() {
        if let Some(index) = records
            .iter()
            .position(|record| matches!(record.body, RecordBody::FinalLen { .. }))
        {
            if index + 1 != records.len() {
                failure = Some(
                    FormatError::new(
                        "EB014",
                        format!(
                            ".geojournal FinalLen 必须位于末条，实际第 {} 条（共 {} 条）",
                            index + 1,
                            records.len()
                        ),
                    )
                    .in_record(index as u64 + 1)
                    .with_recovered(records.len()),
                );
            }
        }
    }

    let partial = failure.is_some();
    Ok(Decoded {
        file: File {
            fmt_ver: header.fmt_ver,
            flags: header.flags,
            doc_uuid: header.doc_uuid,
            base_kind: header.base_kind,
            base_size: header.base_size,
            head_len: header.head_len,
            base_head_blake3: header.base_head_blake3,
            records,
            partial,
        },
        error: failure,
    })
}

/// 解析出的头字段（尚未装入 [`File`]）。
struct Header {
    fmt_ver: u16,
    flags: u16,
    doc_uuid: [u8; 16],
    base_kind: BaseKind,
    base_size: u64,
    head_len: u32,
    base_head_blake3: [u8; 32],
}

/// 校验并解析 80 B 头。头无 CRC；magic/版本/字段范围是唯一防线。
fn parse_header(bytes: &[u8]) -> Result<Header, FormatError> {
    if bytes.len() < MAGIC.len() {
        return Err(FormatError::new(
            "EB013",
            format!(
                ".geojournal 头截断：需要至少 {} B，实际 {} B",
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
                ".geojournal magic 不符：期望 GEOJRN01，实际 {}",
                hex(&bytes[..MAGIC.len()])
            ),
        )
        .at(0));
    }
    if bytes.len() < HEADER_LEN {
        return Err(FormatError::new(
            "EB013",
            format!(
                ".geojournal 头截断：需要 {HEADER_LEN} B，实际 {} B",
                bytes.len()
            ),
        )
        .at(0));
    }

    let fmt_ver = read_u16(bytes, 8);
    if fmt_ver != 1 {
        return Err(FormatError::new(
            "EB001",
            format!(
                ".geojournal 版本过新：fmt_ver={fmt_ver}，本实现只支持 1；\
                 原文件会保留不删，可用 `geode journal export <uuid> -o out.geopatch` 导出可解析前缀"
            ),
        )
        .at(8));
    }
    let flags = read_u16(bytes, 10);
    let mut doc_uuid = [0u8; 16];
    doc_uuid.copy_from_slice(&bytes[12..28]);
    let base_kind = BaseKind::from_byte(bytes[28])?;
    let base_size = read_u64(bytes, 29);
    let head_len = read_u32(bytes, 37);
    let mut base_head_blake3 = [0u8; 32];
    base_head_blake3.copy_from_slice(&bytes[41..73]);

    Ok(Header {
        fmt_ver,
        flags,
        doc_uuid,
        base_kind,
        base_size,
        head_len,
        base_head_blake3,
    })
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
        0x04 => {
            if payload.len() < 2 {
                return Err(FormatError::new(
                    "EB014",
                    format!(
                        "Checkpoint 载荷至少 2 B（label_len），实际 {} B",
                        payload.len()
                    ),
                ));
            }
            let label_len = read_u16(payload, 0) as usize;
            let expected = 2 + label_len;
            if payload.len() != expected {
                return Err(FormatError::new(
                    "EB014",
                    format!(
                        "Checkpoint 载荷长度不符：声明 {label_len} B 标签（共 {expected} B），实际 {} B",
                        payload.len()
                    ),
                ));
            }
            let label = std::str::from_utf8(&payload[2..]).map_err(|error| {
                FormatError::new("EB005", format!("Checkpoint label 不是合法 UTF-8：{error}"))
            })?;
            Ok(RecordBody::Checkpoint {
                label: label.to_owned(),
            })
        }
        0x05 => {
            if payload.len() != 40 {
                return Err(FormatError::new(
                    "EB014",
                    format!(
                        "SaveMark 载荷必须恰好 40 B（new_base_size + blake3），实际 {} B",
                        payload.len()
                    ),
                ));
            }
            let mut new_head_blake3 = [0u8; 32];
            new_head_blake3.copy_from_slice(&payload[8..40]);
            Ok(RecordBody::SaveMark {
                new_base_size: read_u64(payload, 0),
                new_head_blake3,
            })
        }
        0x06 => {
            if payload.len() != 8 {
                return Err(FormatError::new(
                    "EB014",
                    format!("FinalLen 载荷必须恰好 8 B，实际 {} B", payload.len()),
                ));
            }
            Ok(RecordBody::FinalLen {
                len: read_u64(payload, 0),
            })
        }
        other => Ok(RecordBody::Unknown {
            kind: other,
            payload: payload.to_vec(),
        }),
    }
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
        let mut file = File::new(
            [
                0x01, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD, 0xEF, 0x01, 0x23, 0x45, 0x67, 0x89, 0xAB,
                0xCD, 0xEF,
            ],
            BaseKind::File,
            4096,
            65536,
            [0xCD; 32],
        );
        file.records.push(Record::new(
            1,
            RecordBody::Overwrite {
                at: Offset::new(0x10),
                data: vec![1, 2, 3, 4],
            },
        ));
        file.records.push(Record::new(
            2,
            RecordBody::Insert {
                at: Offset::new(0x20),
                data: vec![5, 6],
            },
        ));
        file.records.push(Record::new(
            3,
            RecordBody::Remove {
                at: Offset::new(0x30),
                len: 8,
            },
        ));
        file.records.push(Record::new(
            4,
            RecordBody::Checkpoint {
                label: "paste".to_owned(),
            },
        ));
        file.records.push(Record::new(
            5,
            RecordBody::Unknown {
                kind: 0x7F,
                payload: vec![9, 9],
            },
        ));
        file.records
            .push(Record::new(6, RecordBody::FinalLen { len: 4100 }));
        file
    }

    #[test]
    fn round_trip_all_record_kinds() {
        let file = sample();
        let bytes = encode(&file);
        assert_eq!(decode(&bytes).unwrap(), file);
    }

    #[test]
    fn header_is_80_bytes() {
        let bytes = encode(&sample());
        assert_eq!(&bytes[..8], MAGIC);
        assert_eq!(read_u16(&bytes, 8), 1);
        assert_eq!(bytes[28], 0);
        assert_eq!(bytes.len(), HEADER_LEN + bytes[HEADER_LEN..].len());
    }

    #[test]
    fn bad_magic_is_eb002() {
        let mut bytes = encode(&sample());
        bytes[7] = b'2';
        assert_eq!(decode(&bytes).unwrap_err().code, "EB002");
    }

    #[test]
    fn too_new_version_is_eb001() {
        let mut bytes = encode(&sample());
        bytes[8..10].copy_from_slice(&2u16.to_be_bytes());
        let error = decode(&bytes).unwrap_err();
        assert_eq!(error.code, "EB001");
        assert!(error.detail.contains("journal export"));
    }

    #[test]
    fn truncated_header_and_frame_are_eb013() {
        assert_eq!(decode(b"GEO").unwrap_err().code, "EB013");
        let bytes = encode(&sample());
        assert_eq!(decode(&bytes[..40]).unwrap_err().code, "EB013");
        let error = decode(&bytes[..bytes.len() - 2]).unwrap_err();
        assert_eq!(error.code, "EB013");
        assert_eq!(error.recovered_records, 5);
    }

    #[test]
    fn bad_frame_crc_is_eb011_and_prefix_is_recoverable() {
        let mut bytes = encode(&sample());
        let last = bytes.len() - 1;
        bytes[last] ^= 0xFF;
        let error = decode(&bytes).unwrap_err();
        assert_eq!(error.code, "EB011");
        assert_eq!(error.recovered_records, 5);

        let decoded = decode_prefix(&bytes).unwrap();
        assert!(decoded.is_partial());
        assert!(decoded.file.partial);
        assert_eq!(decoded.file.records.len(), 5);
        assert_eq!(decoded.error.unwrap().code, "EB011");
    }

    #[test]
    fn seq_gap_is_eb012() {
        let mut file = sample();
        file.records[2].seq = 9;
        let bytes = encode(&file);
        let error = decode(&bytes).unwrap_err();
        assert_eq!(error.code, "EB012");
        assert_eq!(error.recovered_records, 2);
    }

    #[test]
    fn final_len_must_be_last() {
        let mut file = File::new([0; 16], BaseKind::Untitled, 0, 4096, [0; 32]);
        file.records
            .push(Record::new(1, RecordBody::FinalLen { len: 0 }));
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
    fn invalid_base_kind_is_eb005() {
        let mut bytes = encode(&sample());
        bytes[28] = 3;
        assert_eq!(decode(&bytes).unwrap_err().code, "EB005");
    }

    #[test]
    fn save_mark_round_trips() {
        let mut file = File::new([0xAA; 16], BaseKind::Provider, 123, 4096, [0xBB; 32]);
        file.records.push(Record::new(
            1,
            RecordBody::SaveMark {
                new_base_size: 456,
                new_head_blake3: [0xCC; 32],
            },
        ));
        file.records
            .push(Record::new(2, RecordBody::FinalLen { len: 456 }));
        assert_eq!(decode(&encode(&file)).unwrap(), file);
    }
}
