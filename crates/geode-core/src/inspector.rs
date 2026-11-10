//! Inspector：全类型解码与反写。
//!
//! DESIGN: 02 §3（类型系统）、10 §4.6（Inspector 值解码）、05 §4.8（Inspector 跟随光标）
//!
//! 本模块只做**纯函数**：字节 + 类型 + 字节序 → [`Value`]，以及反向编码。
//! 不读文件、不持有状态，因此 UI 可以在每次光标移动后全量重解而不必担心副作用。
//! 所有解码在短读时返回 [`InspectError::ShortRead`]，**绝不补零、绝不 panic**。

use crate::value::Value;
use core::fmt;

/// Inspector 可解码的定宽标量类型。
///
/// 与 02 §3.1 一一对应；`Bytes(u32)` 表示"原始字节组"，长度由参数给出
/// （它不是 BinSpec 的数组语法，数组由解析器处理）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Type {
    /// 8 位无符号整数。
    U8,
    /// 16 位无符号整数。
    U16,
    /// 24 位无符号整数（3 字节，PE/DVB 等格式真实存在）。
    U24,
    /// 32 位无符号整数。
    U32,
    /// 48 位无符号整数（6 字节）。
    U48,
    /// 64 位无符号整数。
    U64,
    /// 128 位无符号整数。
    U128,
    /// 8 位有符号整数。
    I8,
    /// 16 位有符号整数。
    I16,
    /// 24 位有符号整数（符号扩展到 32 位）。
    I24,
    /// 32 位有符号整数。
    I32,
    /// 48 位有符号整数（符号扩展到 64 位）。
    I48,
    /// 64 位有符号整数。
    I64,
    /// 128 位有符号整数。
    I128,
    /// IEEE-754 半精度浮点（按原始 16 位保存）。
    F16,
    /// IEEE-754 单精度浮点（按原始 32 位保存）。
    F32,
    /// IEEE-754 双精度浮点（按原始 64 位保存）。
    F64,
    /// 布尔：1 字节，0 = false，非 0 = true。
    Bool,
    /// 单个 UTF-8 字符：1 字节（见 [`decode`] 的错误说明）。
    Char,
    /// 定长原始字节组，参数为字节数。
    Bytes(u32),
}

impl Type {
    /// 该类型占用的字节数。
    ///
    /// 固定宽度类型的返回值与 02 §3.1 一致；[`Type::Bytes`] 返回其参数。
    pub const fn size(self) -> usize {
        match self {
            Self::U8 | Self::I8 | Self::Bool | Self::Char => 1,
            Self::U16 | Self::I16 | Self::F16 => 2,
            Self::U24 | Self::I24 => 3,
            Self::U32 | Self::I32 | Self::F32 => 4,
            Self::U48 | Self::I48 => 6,
            Self::U64 | Self::I64 | Self::F64 => 8,
            Self::U128 | Self::I128 => 16,
            Self::Bytes(n) => n as usize,
        }
    }

    /// 是否无符号整数类型（仅用于内部编码分派）。
    const fn is_unsigned(self) -> bool {
        matches!(
            self,
            Self::U8 | Self::U16 | Self::U24 | Self::U32 | Self::U48 | Self::U64 | Self::U128
        )
    }

    /// 是否有符号整数类型（仅用于内部编码分派）。
    const fn is_signed(self) -> bool {
        matches!(
            self,
            Self::I8 | Self::I16 | Self::I24 | Self::I32 | Self::I48 | Self::I64 | Self::I128
        )
    }

    /// 规范的 BinSpec 类型名，用于错误文案。
    const fn name(self) -> &'static str {
        match self {
            Self::U8 => "u8",
            Self::U16 => "u16",
            Self::U24 => "u24",
            Self::U32 => "u32",
            Self::U48 => "u48",
            Self::U64 => "u64",
            Self::U128 => "u128",
            Self::I8 => "i8",
            Self::I16 => "i16",
            Self::I24 => "i24",
            Self::I32 => "i32",
            Self::I48 => "i48",
            Self::I64 => "i64",
            Self::I128 => "i128",
            Self::F16 => "f16",
            Self::F32 => "f32",
            Self::F64 => "f64",
            Self::Bool => "bool",
            Self::Char => "char",
            Self::Bytes(_) => "bytes",
        }
    }
}

/// 字节序。作为**入参**而非状态：切换只是重调纯函数（10 §4.6）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Endian {
    /// 小端：最低有效字节在前。
    Little,
    /// 大端：最高有效字节在前。
    Big,
}

/// Inspector 解码/编码错误。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InspectError {
    /// 输入字节不足以解码目标类型（**不补零**）。
    ShortRead {
        /// 目标类型需要的字节数。
        need: usize,
        /// 实际提供的字节数。
        got: usize,
    },
    /// 字节序列不是合法的 UTF-8 单字符。
    BadChar,
    /// 值超出目标类型可表示范围（也用于 `Bytes` 长度不符）。
    OutOfRange,
    /// LEB128 编码超过 10 字节或数值溢出。
    LebOverflow,
    /// 值变体与目标类型不匹配。
    TypeMismatch {
        /// 期望的 BinSpec 类型名，例如 `"u8"`。
        expected: &'static str,
    },
}

impl InspectError {
    /// 映射到已有诊断码（`docs/errors.toml`）。
    ///
    /// - 短读 → `E3001`（读取越过文件末尾），语义完全一致；
    /// - 非法字符 → `E1002`（字节序列不是该编码的合法文本），语义完全一致；
    /// - 其余三种目前**没有专用码**：值域/长度错误、LEB128 溢出、类型不匹配
    ///   都是 Inspector 内部的输入校验错误，错误码表里没有对应条目，
    ///   按任务约定回落到 `E1002`。将来补码时应在此处替换。
    pub fn code(&self) -> &'static str {
        match self {
            Self::ShortRead { .. } => "E3001",
            Self::BadChar => "E1002",
            Self::OutOfRange | Self::LebOverflow | Self::TypeMismatch { .. } => "E1002",
        }
    }
}

impl fmt::Display for InspectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ShortRead { need, got } => {
                write!(f, "短读：需要 {need} 字节，实际只有 {got} 字节")
            }
            Self::BadChar => f.write_str("字节不是合法的 UTF-8 单字符"),
            Self::OutOfRange => f.write_str("值超出目标类型可表示范围"),
            Self::LebOverflow => f.write_str("LEB128 编码超过 10 字节或数值溢出"),
            Self::TypeMismatch { expected } => {
                write!(f, "值类型与目标类型不匹配（期望 {expected}）")
            }
        }
    }
}

impl std::error::Error for InspectError {}

/// 从 `bytes` 的前 `ty.size()` 个字节解码一个值。
///
/// - 输入比类型短时返回 [`InspectError::ShortRead`]：`need` 是类型大小，
///   `got` 是实际长度；**绝不补零**，例如 `decode(&[0xFF], Type::U16, _)` 是错误，
///   而不是 `0x00FF`。
/// - 多余字节被忽略（调用方通常只切出光标处的一小段）。
/// - 浮点保存**原始位**（`F16Bits`/`F32Bits`/`F64Bits`），因此 NaN payload、
///   `-0.0` 与非规格化数都能原样往返；f16 的显示转换复用 [`Value`] 的 `Display`
///   （内部即 `value.rs` 的 `half_to_f32`）。
/// - `char` 按 02 §3.1 固定占 1 字节，按 UTF-8 解码：ASCII 合法，孤立高位字节
///   （0x80..=0xFF）返回 [`InspectError::BadChar`]。
pub fn decode(bytes: &[u8], ty: Type, endian: Endian) -> Result<Value, InspectError> {
    let need = ty.size();
    if bytes.len() < need {
        return Err(InspectError::ShortRead {
            need,
            got: bytes.len(),
        });
    }
    let raw = &bytes[..need];
    Ok(match ty {
        Type::U8 | Type::U16 | Type::U24 | Type::U32 | Type::U48 | Type::U64 | Type::U128 => {
            Value::U(assemble(raw, endian))
        }
        Type::I8 | Type::I16 | Type::I24 | Type::I32 | Type::I48 | Type::I64 | Type::I128 => {
            Value::I(sign_extend(assemble(raw, endian), need * 8))
        }
        Type::F16 => Value::F16Bits(assemble(raw, endian) as u16),
        Type::F32 => Value::F32Bits(assemble(raw, endian) as u32),
        Type::F64 => Value::F64Bits(assemble(raw, endian) as u64),
        Type::Bool => Value::Bool(raw[0] != 0),
        Type::Char => {
            let s = core::str::from_utf8(raw).map_err(|_| InspectError::BadChar)?;
            let mut chars = s.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => Value::Char(c),
                _ => return Err(InspectError::BadChar),
            }
        }
        Type::Bytes(_) => Value::Bytes(raw.to_vec()),
    })
}

/// 把值编码为 `ty` 的定宽字节序列，与 [`decode`] 严格互逆。
///
/// - 值越界（如 `Value::U(300)` 写入 `u8`）返回 [`InspectError::OutOfRange`]；
/// - 值变体与类型不符（如用 `Value::I` 写 `u32`）返回
///   [`InspectError::TypeMismatch`]；
/// - `bool` 只有一种规范编码：`true → 0x01`、`false → 0x00`。非零字节解码为
///   `true` 后回写为 `1`，这是该类型唯一允许的"非逐位往返"（语义往返成立）。
/// - `char` 只有单字节 UTF-8（ASCII）可编码，其余返回 [`InspectError::OutOfRange`]。
pub fn encode(value: &Value, ty: Type, endian: Endian) -> Result<Vec<u8>, InspectError> {
    if ty.is_unsigned() {
        let Value::U(v) = value else {
            return Err(InspectError::TypeMismatch {
                expected: ty.name(),
            });
        };
        return encode_uint(*v, ty.size() * 8, endian);
    }
    if ty.is_signed() {
        let Value::I(v) = value else {
            return Err(InspectError::TypeMismatch {
                expected: ty.name(),
            });
        };
        return encode_int(*v, ty.size() * 8, endian);
    }
    match (ty, value) {
        (Type::F16, Value::F16Bits(bits)) => Ok(write_uint(u128::from(*bits), 2, endian)),
        (Type::F32, Value::F32Bits(bits)) => Ok(write_uint(u128::from(*bits), 4, endian)),
        (Type::F64, Value::F64Bits(bits)) => Ok(write_uint(u128::from(*bits), 8, endian)),
        (Type::Bool, Value::Bool(b)) => Ok(vec![u8::from(*b)]),
        (Type::Char, Value::Char(c)) => {
            let mut buf = [0u8; 4];
            let s = c.encode_utf8(&mut buf);
            if s.len() == 1 {
                Ok(vec![buf[0]])
            } else {
                Err(InspectError::OutOfRange)
            }
        }
        (Type::Bytes(n), Value::Bytes(b)) => {
            if b.len() == n as usize {
                Ok(b.clone())
            } else {
                Err(InspectError::OutOfRange)
            }
        }
        (t, _) => Err(InspectError::TypeMismatch { expected: t.name() }),
    }
}

/// Windows GUID 的混合端序解码，返回 `{XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX}`。
///
/// 字节布局（10 §4.6 的"Windows 坑"）：
/// `Data1: u32 LE`、`Data2: u16 LE`、`Data3: u16 LE`、`Data4: [u8; 8]` 原样。
/// 因此 `33 22 11 00 55 44 77 66 88 99 AA BB CC DD EE FF` 对应
/// `{00112233-4455-6677-8899-AABBCCDDEEFF}`。不足 16 字节返回
/// [`InspectError::ShortRead`]。
pub fn decode_guid(bytes: &[u8]) -> Result<String, InspectError> {
    if bytes.len() < 16 {
        return Err(InspectError::ShortRead {
            need: 16,
            got: bytes.len(),
        });
    }
    let data1 = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let data2 = u16::from_le_bytes([bytes[4], bytes[5]]);
    let data3 = u16::from_le_bytes([bytes[6], bytes[7]]);
    let tail = format!(
        "{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
        bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]
    );
    Ok(format!("{{{data1:08X}-{data2:04X}-{data3:04X}-{tail}}}"))
}

/// 解码 LEB128，返回 `(值, 消费字节数)`。
///
/// - `signed = true` 时按有符号 LEB128 做符号扩展；
/// - 最多 10 字节（10 §4.6 的上限）。第 10 字节仍带续位、或输入超过 10 字节，
///   返回 [`InspectError::LebOverflow`]；10 字节最多承载 70 个有效位，必然落在
///   `i128` 范围内，因此不存在"数值超出 i128"的路径；
/// - 输入在续位中途耗尽返回 [`InspectError::ShortRead`]（`need = got + 1`）；
/// - 非规范编码（例如 `[0x80, 0x00]` 表示 0）仍被接受并返回消费字节数，
///   10 §4.6 要求 UI 另行标注 `NonCanonical`，本函数不产生警告类型。
pub fn decode_leb128(bytes: &[u8], signed: bool) -> Result<(i128, usize), InspectError> {
    let mut result: u128 = 0;
    for (i, &byte) in bytes.iter().enumerate() {
        if i >= 10 {
            return Err(InspectError::LebOverflow);
        }
        result |= u128::from(byte & 0x7F) << (7 * i);
        if byte & 0x80 == 0 {
            let consumed = i + 1;
            let value = if signed && byte & 0x40 != 0 {
                // 把已用的 7*n 位之外的位全部填 1，得到二补码负数。
                let used_bits = 7 * consumed;
                (result | (!0u128 << used_bits)) as i128
            } else {
                result as i128
            };
            return Ok((value, consumed));
        }
    }
    if bytes.len() >= 10 {
        Err(InspectError::LebOverflow)
    } else {
        Err(InspectError::ShortRead {
            need: bytes.len() + 1,
            got: bytes.len(),
        })
    }
}

/// 时间戳的编码种类（全部换算到 Unix 秒，不做时区）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TimestampKind {
    /// Unix 秒，`u32` 小端（4 字节）。
    Unix32Le,
    /// Unix 秒，`u64` 小端（8 字节）。
    Unix64Le,
    /// Unix 秒，`u64` 大端（8 字节）。
    Unix64Be,
    /// Windows FILETIME：自 1601-01-01 UTC 起的 100ns 数，`u64` 小端（8 字节）。
    WindowsFileTime,
    /// Mac HFS：自 1904-01-01 UTC 起的秒数，`u32` 大端（4 字节）。
    MacHfs,
}

impl TimestampKind {
    /// 该时间戳编码占用的字节数。
    const fn size(self) -> usize {
        match self {
            Self::Unix32Le | Self::MacHfs => 4,
            Self::Unix64Le | Self::Unix64Be | Self::WindowsFileTime => 8,
        }
    }
}

/// 解码时间戳为 Unix 秒（1970-01-01 UTC 起）。
///
/// - `WindowsFileTime`：`ft / 10_000_000 - 11_644_473_600`；
/// - `MacHfs`：`hfs - 2_082_844_800`；
/// - 只做换算，不做时区/日期格式化；负值表示 1970 年之前，合法；
/// - `Unix64*` 的值超过 `i64::MAX` 时返回 [`InspectError::OutOfRange`]。
pub fn decode_timestamp(bytes: &[u8], kind: TimestampKind) -> Result<i64, InspectError> {
    let need = kind.size();
    if bytes.len() < need {
        return Err(InspectError::ShortRead {
            need,
            got: bytes.len(),
        });
    }
    Ok(match kind {
        TimestampKind::Unix32Le => {
            i64::from(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
        }
        TimestampKind::Unix64Le => {
            let raw = u64::from_le_bytes([
                bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
            ]);
            i64::try_from(raw).map_err(|_| InspectError::OutOfRange)?
        }
        TimestampKind::Unix64Be => {
            let raw = u64::from_be_bytes([
                bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
            ]);
            i64::try_from(raw).map_err(|_| InspectError::OutOfRange)?
        }
        TimestampKind::WindowsFileTime => {
            let ft = u64::from_le_bytes([
                bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
            ]);
            let secs = i128::from(ft / 10_000_000) - 11_644_473_600i128;
            i64::try_from(secs).map_err(|_| InspectError::OutOfRange)?
        }
        TimestampKind::MacHfs => {
            let secs = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
            i64::from(secs) - 2_082_844_800i64
        }
    })
}

/// RGBA8 颜色（每通道 8 位）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgba8 {
    /// 红通道。
    pub r: u8,
    /// 绿通道。
    pub g: u8,
    /// 蓝通道。
    pub b: u8,
    /// Alpha 通道（255 = 不透明）。
    pub a: u8,
}

impl Rgba8 {
    /// 返回 `#RRGGBBAA` 形式的大写十六进制字符串。
    pub fn to_hex(self) -> String {
        format!("#{:02X}{:02X}{:02X}{:02X}", self.r, self.g, self.b, self.a)
    }
}

/// 解码 4 字节 RGBA8（字节序即 `R,G,B,A`，不做通道重排）。
pub fn decode_rgba8(bytes: [u8; 4]) -> Rgba8 {
    Rgba8 {
        r: bytes[0],
        g: bytes[1],
        b: bytes[2],
        a: bytes[3],
    }
}

/// 按字节序把 `bytes` 拼成无符号整数（`bytes` 长度 1..=16）。
fn assemble(bytes: &[u8], endian: Endian) -> u128 {
    match endian {
        Endian::Little => bytes
            .iter()
            .enumerate()
            .fold(0u128, |acc, (i, &b)| acc | (u128::from(b) << (8 * i))),
        Endian::Big => bytes
            .iter()
            .fold(0u128, |acc, &b| (acc << 8) | u128::from(b)),
    }
}

/// 把 `bits` 位二补码数符号扩展到 128 位。
fn sign_extend(raw: u128, bits: usize) -> i128 {
    let shift = 128 - bits;
    ((raw << shift) as i128) >> shift
}

/// 把无符号整数写成 `len` 字节。
fn write_uint(value: u128, len: usize, endian: Endian) -> Vec<u8> {
    match endian {
        Endian::Little => value.to_le_bytes()[..len].to_vec(),
        Endian::Big => value.to_be_bytes()[16 - len..].to_vec(),
    }
}

/// 编码无符号整数，越界返回 [`InspectError::OutOfRange`]。
fn encode_uint(value: u128, bits: usize, endian: Endian) -> Result<Vec<u8>, InspectError> {
    if bits < 128 && value >> bits != 0 {
        return Err(InspectError::OutOfRange);
    }
    Ok(write_uint(value, bits / 8, endian))
}

/// 编码有符号整数，越界返回 [`InspectError::OutOfRange`]。
fn encode_int(value: i128, bits: usize, endian: Endian) -> Result<Vec<u8>, InspectError> {
    if bits < 128 {
        let min = i128::MIN >> (128 - bits);
        let max = i128::MAX >> (128 - bits);
        if value < min || value > max {
            return Err(InspectError::OutOfRange);
        }
    }
    Ok(write_uint(value as u128, bits / 8, endian))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// 双端序往返：encode → decode 必须得到原值，且长度等于 `size()`。
    fn round_trip(value: Value, ty: Type) {
        for endian in [Endian::Little, Endian::Big] {
            let bytes = encode(&value, ty, endian).expect("编码应成功");
            assert_eq!(bytes.len(), ty.size(), "{ty:?} 的编码长度");
            let back = decode(&bytes, ty, endian).expect("解码应成功");
            assert_eq!(back, value, "{ty:?} / {endian:?} 往返");
        }
    }

    #[test]
    fn type_sizes_match_spec() {
        let cases = [
            (Type::U8, 1),
            (Type::U16, 2),
            (Type::U24, 3),
            (Type::U32, 4),
            (Type::U48, 6),
            (Type::U64, 8),
            (Type::U128, 16),
            (Type::I8, 1),
            (Type::I16, 2),
            (Type::I24, 3),
            (Type::I32, 4),
            (Type::I48, 6),
            (Type::I64, 8),
            (Type::I128, 16),
            (Type::F16, 2),
            (Type::F32, 4),
            (Type::F64, 8),
            (Type::Bool, 1),
            (Type::Char, 1),
            (Type::Bytes(7), 7),
            (Type::Bytes(0), 0),
        ];
        for (ty, size) in cases {
            assert_eq!(ty.size(), size, "{ty:?}");
        }
    }

    #[test]
    fn unsigned_round_trip_all_widths() {
        round_trip(Value::U(u128::from(u8::MAX)), Type::U8);
        round_trip(Value::U(u128::from(u16::MAX)), Type::U16);
        round_trip(Value::U(0x00FF_FFFF), Type::U24);
        round_trip(Value::U(u128::from(u32::MAX)), Type::U32);
        round_trip(Value::U(0x0000_FFFF_FFFF_FFFF), Type::U48);
        round_trip(Value::U(u128::from(u64::MAX)), Type::U64);
        round_trip(Value::U(u128::MAX), Type::U128);
    }

    #[test]
    fn signed_round_trip_all_widths() {
        round_trip(Value::I(i128::from(i8::MIN)), Type::I8);
        round_trip(Value::I(i128::from(i16::MAX)), Type::I16);
        round_trip(Value::I(-8_388_608), Type::I24);
        round_trip(Value::I(8_388_607), Type::I24);
        round_trip(Value::I(i128::from(i32::MIN)), Type::I32);
        round_trip(Value::I(-140_737_488_355_328), Type::I48);
        round_trip(Value::I(140_737_488_355_327), Type::I48);
        round_trip(Value::I(i128::from(i64::MIN)), Type::I64);
        round_trip(Value::I(i128::MIN), Type::I128);
        round_trip(Value::I(i128::MAX), Type::I128);
    }

    #[test]
    fn unsigned_24_48_wire_order() {
        assert_eq!(
            decode(&[0x01, 0x02, 0x03], Type::U24, Endian::Little),
            Ok(Value::U(0x030201))
        );
        assert_eq!(
            decode(&[0x01, 0x02, 0x03], Type::U24, Endian::Big),
            Ok(Value::U(0x010203))
        );
        assert_eq!(
            decode(&[1, 2, 3, 4, 5, 6], Type::U48, Endian::Little),
            Ok(Value::U(0x060504030201))
        );
        assert_eq!(
            decode(&[1, 2, 3, 4, 5, 6], Type::U48, Endian::Big),
            Ok(Value::U(0x010203040506))
        );
    }

    #[test]
    fn i24_sign_extension() {
        assert_eq!(
            decode(&[0xFF, 0xFF, 0xFF], Type::I24, Endian::Little),
            Ok(Value::I(-1))
        );
        assert_eq!(
            decode(&[0x00, 0x00, 0x80], Type::I24, Endian::Little),
            Ok(Value::I(-8_388_608))
        );
        assert_eq!(
            decode(&[0x80, 0x00, 0x00], Type::I24, Endian::Big),
            Ok(Value::I(-8_388_608))
        );
        assert_eq!(
            decode(&[0xFF, 0xFF, 0x7F], Type::I24, Endian::Little),
            Ok(Value::I(8_388_607))
        );
        assert_eq!(
            decode(&[0x00, 0x00, 0x40], Type::I24, Endian::Little),
            Ok(Value::I(4_194_304))
        );
    }

    #[test]
    fn i48_sign_extension() {
        assert_eq!(
            decode(&[0xFF; 6], Type::I48, Endian::Little),
            Ok(Value::I(-1))
        );
        assert_eq!(
            decode(
                &[0x00, 0x00, 0x00, 0x00, 0x00, 0x80],
                Type::I48,
                Endian::Little
            ),
            Ok(Value::I(-140_737_488_355_328))
        );
        assert_eq!(
            decode(
                &[0x80, 0x00, 0x00, 0x00, 0x00, 0x00],
                Type::I48,
                Endian::Big
            ),
            Ok(Value::I(-140_737_488_355_328))
        );
        assert_eq!(
            decode(
                &[0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x7F],
                Type::I48,
                Endian::Little
            ),
            Ok(Value::I(140_737_488_355_327))
        );
    }

    #[test]
    fn float_round_trip_preserves_bits() {
        round_trip(Value::F16Bits(0x3C00), Type::F16);
        round_trip(Value::F32Bits(0x7FC0_0001), Type::F32);
        round_trip(Value::F64Bits(0x7FF8_0000_0000_0001), Type::F64);
        round_trip(Value::F32Bits(0x8000_0000), Type::F32);
        round_trip(Value::F64Bits(0x0000_0000_0000_0001), Type::F64);
    }

    #[test]
    fn f16_special_values_display_via_value() {
        let half =
            |bits: u16| decode(&bits.to_le_bytes(), Type::F16, Endian::Little).expect("2 字节足够");
        assert_eq!(half(0x3C00), Value::F16Bits(0x3C00));
        assert_eq!(half(0x3C00).to_string(), "1");
        assert_eq!(half(0x8000).to_string(), "-0");
        assert!(half(0x7C00).to_string().contains("inf"));
        assert!(half(0x7E00).to_string().contains("NaN"));
        // 最小非规格化数：显示绝不能是 0。
        assert_ne!(half(0x0001).to_string(), "0");
    }

    #[test]
    fn bool_round_trip_and_nonzero_truth() {
        round_trip(Value::Bool(true), Type::Bool);
        round_trip(Value::Bool(false), Type::Bool);
        assert_eq!(
            decode(&[0x00], Type::Bool, Endian::Little),
            Ok(Value::Bool(false))
        );
        assert_eq!(
            decode(&[0x02], Type::Bool, Endian::Little),
            Ok(Value::Bool(true))
        );
        assert_eq!(
            encode(&Value::Bool(true), Type::Bool, Endian::Little),
            Ok(vec![1])
        );
    }

    #[test]
    fn char_round_trip_and_invalid() {
        round_trip(Value::Char('A'), Type::Char);
        assert_eq!(
            decode(&[0xFF], Type::Char, Endian::Little),
            Err(InspectError::BadChar)
        );
        // 多字节 UTF-8 字符装不进 1 字节的 char。
        assert_eq!(
            encode(&Value::Char('中'), Type::Char, Endian::Little),
            Err(InspectError::OutOfRange)
        );
        assert_eq!(
            decode(&[0xC3, 0xA9], Type::Char, Endian::Little),
            Err(InspectError::BadChar)
        );
    }

    #[test]
    fn bytes_round_trip_and_length_check() {
        round_trip(Value::Bytes(vec![0xDE, 0xAD, 0xBE]), Type::Bytes(3));
        round_trip(Value::Bytes(Vec::new()), Type::Bytes(0));
        assert_eq!(
            decode(&[1, 2], Type::Bytes(4), Endian::Little),
            Err(InspectError::ShortRead { need: 4, got: 2 })
        );
        assert_eq!(
            encode(&Value::Bytes(vec![1, 2]), Type::Bytes(3), Endian::Little),
            Err(InspectError::OutOfRange)
        );
    }

    #[test]
    fn short_read_never_zero_pads() {
        assert_eq!(
            decode(&[], Type::U32, Endian::Little),
            Err(InspectError::ShortRead { need: 4, got: 0 })
        );
        assert_eq!(
            decode(&[0xFF], Type::U16, Endian::Little),
            Err(InspectError::ShortRead { need: 2, got: 1 })
        );
        assert_eq!(
            decode(&[0xFF; 15], Type::U128, Endian::Big),
            Err(InspectError::ShortRead { need: 16, got: 15 })
        );
        // 这条断言是"不补零"的核心：若实现补零会得到 Ok(U(0x00FF))。
        assert!(decode(&[0xFF], Type::U16, Endian::Little).is_err());
    }

    #[test]
    fn trailing_bytes_are_ignored() {
        assert_eq!(
            decode(&[0x01, 0x02, 0xAA, 0xBB], Type::U16, Endian::Little),
            Ok(Value::U(0x0201))
        );
        assert_eq!(
            decode(&[0x01, 0x02, 0xAA, 0xBB], Type::U16, Endian::Big),
            Ok(Value::U(0x0102))
        );
    }

    #[test]
    fn encode_rejects_out_of_range() {
        assert_eq!(
            encode(&Value::U(300), Type::U8, Endian::Little),
            Err(InspectError::OutOfRange)
        );
        assert_eq!(
            encode(&Value::U(256), Type::U8, Endian::Little),
            Err(InspectError::OutOfRange)
        );
        assert_eq!(
            encode(&Value::I(128), Type::I8, Endian::Little),
            Err(InspectError::OutOfRange)
        );
        assert_eq!(
            encode(&Value::I(-129), Type::I8, Endian::Little),
            Err(InspectError::OutOfRange)
        );
        assert_eq!(
            encode(&Value::U(1 << 24), Type::U24, Endian::Little),
            Err(InspectError::OutOfRange)
        );
        assert_eq!(
            encode(&Value::U(1 << 48), Type::U48, Endian::Little),
            Err(InspectError::OutOfRange)
        );
        assert_eq!(
            encode(&Value::I(1 << 47), Type::I48, Endian::Little),
            Err(InspectError::OutOfRange)
        );
    }

    #[test]
    fn encode_rejects_type_mismatch() {
        assert_eq!(
            encode(&Value::I(1), Type::U8, Endian::Little),
            Err(InspectError::TypeMismatch { expected: "u8" })
        );
        assert_eq!(
            encode(&Value::U(1), Type::F32, Endian::Little),
            Err(InspectError::TypeMismatch { expected: "f32" })
        );
        assert_eq!(
            encode(&Value::Bool(true), Type::Char, Endian::Little),
            Err(InspectError::TypeMismatch { expected: "char" })
        );
    }

    #[test]
    fn guid_known_windows_byte_order() {
        let windows = [
            0x33, 0x22, 0x11, 0x00, 0x55, 0x44, 0x77, 0x66, 0x88, 0x99, 0xAA, 0xBB, 0xCC, 0xDD,
            0xEE, 0xFF,
        ];
        assert_eq!(
            decode_guid(&windows),
            Ok("{00112233-4455-6677-8899-AABBCCDDEEFF}".to_owned())
        );
        // 同样 16 字节若按 RFC 4122 原始序解释，前三段会反过来——证明混合端序生效。
        let rfc = [
            0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xAA, 0xBB, 0xCC, 0xDD,
            0xEE, 0xFF,
        ];
        assert_eq!(
            decode_guid(&rfc),
            Ok("{33221100-5544-7766-8899-AABBCCDDEEFF}".to_owned())
        );
    }

    #[test]
    fn guid_short_read() {
        assert_eq!(
            decode_guid(&[0u8; 15]),
            Err(InspectError::ShortRead { need: 16, got: 15 })
        );
        assert_eq!(
            decode_guid(&[]),
            Err(InspectError::ShortRead { need: 16, got: 0 })
        );
    }

    #[test]
    fn leb128_unsigned_known_vectors() {
        assert_eq!(decode_leb128(&[0x00], false), Ok((0, 1)));
        assert_eq!(decode_leb128(&[0x7F], false), Ok((127, 1)));
        assert_eq!(decode_leb128(&[0x80, 0x01], false), Ok((128, 2)));
        assert_eq!(decode_leb128(&[0xE5, 0x8E, 0x26], false), Ok((624_485, 3)));
        let u64_max = [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x01];
        assert_eq!(
            decode_leb128(&u64_max, false),
            Ok((i128::from(u64::MAX), 10))
        );
    }

    #[test]
    fn leb128_signed_known_vectors() {
        assert_eq!(decode_leb128(&[0x00], true), Ok((0, 1)));
        assert_eq!(decode_leb128(&[0x7F], true), Ok((-1, 1)));
        assert_eq!(decode_leb128(&[0x3F], true), Ok((63, 1)));
        assert_eq!(decode_leb128(&[0xC0, 0x00], true), Ok((64, 2)));
        assert_eq!(decode_leb128(&[0x40], true), Ok((-64, 1)));
        assert_eq!(decode_leb128(&[0xBF, 0x7F], true), Ok((-65, 2)));
        assert_eq!(decode_leb128(&[0xC0, 0xBB, 0x78], true), Ok((-123_456, 3)));
        // 非规范但可接受：0 用了两个字节。
        assert_eq!(decode_leb128(&[0x80, 0x00], true), Ok((0, 2)));
    }

    #[test]
    fn leb128_overflow_and_short_read() {
        assert_eq!(
            decode_leb128(&[], false),
            Err(InspectError::ShortRead { need: 1, got: 0 })
        );
        assert_eq!(
            decode_leb128(&[0x80], false),
            Err(InspectError::ShortRead { need: 2, got: 1 })
        );
        assert_eq!(
            decode_leb128(&[0x80; 10], false),
            Err(InspectError::LebOverflow)
        );
        assert_eq!(
            decode_leb128(&[0x80; 11], true),
            Err(InspectError::LebOverflow)
        );
    }

    #[test]
    fn timestamp_unix_variants() {
        assert_eq!(
            decode_timestamp(&0u32.to_le_bytes(), TimestampKind::Unix32Le),
            Ok(0)
        );
        assert_eq!(
            decode_timestamp(&u32::MAX.to_le_bytes(), TimestampKind::Unix32Le),
            Ok(i64::from(u32::MAX))
        );
        let secs = 1_700_000_000u64;
        assert_eq!(
            decode_timestamp(&secs.to_le_bytes(), TimestampKind::Unix64Le),
            Ok(1_700_000_000)
        );
        assert_eq!(
            decode_timestamp(&secs.to_be_bytes(), TimestampKind::Unix64Be),
            Ok(1_700_000_000)
        );
        assert_eq!(
            decode_timestamp(&u64::MAX.to_le_bytes(), TimestampKind::Unix64Le),
            Err(InspectError::OutOfRange)
        );
    }

    #[test]
    fn timestamp_filetime_and_hfs() {
        // FILETIME 0 = 1601-01-01；Unix 纪元 = 11644473600 秒后。
        assert_eq!(
            decode_timestamp(&0u64.to_le_bytes(), TimestampKind::WindowsFileTime),
            Ok(-11_644_473_600)
        );
        assert_eq!(
            decode_timestamp(
                &116_444_736_000_000_000u64.to_le_bytes(),
                TimestampKind::WindowsFileTime
            ),
            Ok(0)
        );
        assert_eq!(
            decode_timestamp(
                &116_444_736_010_000_000u64.to_le_bytes(),
                TimestampKind::WindowsFileTime
            ),
            Ok(1)
        );
        // HFS 0 = 1904-01-01；Unix 纪元 = 2082844800 秒后。
        assert_eq!(
            decode_timestamp(&0u32.to_be_bytes(), TimestampKind::MacHfs),
            Ok(-2_082_844_800)
        );
        assert_eq!(
            decode_timestamp(&2_082_844_800u32.to_be_bytes(), TimestampKind::MacHfs),
            Ok(0)
        );
        assert_eq!(
            decode_timestamp(&2_082_844_801u32.to_be_bytes(), TimestampKind::MacHfs),
            Ok(1)
        );
    }

    #[test]
    fn timestamp_short_read() {
        assert_eq!(
            decode_timestamp(&[0; 3], TimestampKind::Unix32Le),
            Err(InspectError::ShortRead { need: 4, got: 3 })
        );
        assert_eq!(
            decode_timestamp(&[0; 7], TimestampKind::WindowsFileTime),
            Err(InspectError::ShortRead { need: 8, got: 7 })
        );
    }

    #[test]
    fn rgba8_fields_and_hex() {
        let c = decode_rgba8([0x12, 0x34, 0x56, 0x78]);
        assert_eq!(
            c,
            Rgba8 {
                r: 0x12,
                g: 0x34,
                b: 0x56,
                a: 0x78
            }
        );
        assert_eq!(c.to_hex(), "#12345678");
        assert_eq!(decode_rgba8([0, 0, 0, 0]).to_hex(), "#00000000");
        assert_eq!(decode_rgba8([0xFF, 0xFF, 0xFF, 0xFF]).to_hex(), "#FFFFFFFF");
    }

    #[test]
    fn error_display_codes_and_std_error() {
        assert_eq!(InspectError::ShortRead { need: 4, got: 2 }.code(), "E3001");
        assert_eq!(InspectError::BadChar.code(), "E1002");
        assert_eq!(InspectError::OutOfRange.code(), "E1002");
        assert_eq!(InspectError::LebOverflow.code(), "E1002");
        assert_eq!(
            InspectError::TypeMismatch { expected: "u8" }.code(),
            "E1002"
        );
        assert!(InspectError::BadChar.to_string().contains("UTF-8"));
        let dyn_err: &dyn std::error::Error = &InspectError::OutOfRange;
        assert!(!dyn_err.to_string().is_empty());
    }

    const ALL_TYPES: &[Type] = &[
        Type::U8,
        Type::U16,
        Type::U24,
        Type::U32,
        Type::U48,
        Type::U64,
        Type::U128,
        Type::I8,
        Type::I16,
        Type::I24,
        Type::I32,
        Type::I48,
        Type::I64,
        Type::I128,
        Type::F16,
        Type::F32,
        Type::F64,
        Type::Bool,
        Type::Char,
        Type::Bytes(0),
        Type::Bytes(3),
        Type::Bytes(16),
        Type::Bytes(u32::MAX),
    ];

    fn any_type() -> impl Strategy<Value = Type> {
        prop::sample::select(ALL_TYPES)
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        /// 任意 u128 值、任意字节序都必须原样往返。
        #[test]
        fn prop_u128_round_trip(value: u128, big_endian: bool) {
            let endian = if big_endian { Endian::Big } else { Endian::Little };
            let bytes = encode(&Value::U(value), Type::U128, endian)
                .expect("u128 总是可编码");
            prop_assert_eq!(bytes.len(), 16);
            prop_assert_eq!(
                decode(&bytes, Type::U128, endian).expect("16 字节足够"),
                Value::U(value)
            );
        }

        /// 任意字节切片 + 任意类型都必须返回 Ok/Err，绝不能 panic。
        #[test]
        fn prop_decode_arbitrary_bytes_never_panics(
            bytes in prop::collection::vec(any::<u8>(), 0..48),
            ty in any_type(),
            big_endian: bool,
        ) {
            let endian = if big_endian { Endian::Big } else { Endian::Little };
            let _ = decode(&bytes, ty, endian);
            let _ = decode_guid(&bytes);
        }

        /// LEB128 对任意输入也不得 panic。
        #[test]
        fn prop_leb128_arbitrary_bytes_never_panics(
            bytes in prop::collection::vec(any::<u8>(), 0..24),
            signed: bool,
        ) {
            let _ = decode_leb128(&bytes, signed);
        }
    }
}
