//! 数值范围查询：把整数/浮点区间编译成逐字节比较器。
//!
//! 浮点使用 IEEE-754 totalOrder 键：
//! `key = if sign { !bits } else { bits | 0x8000_0000 }`。
//! 该映射是单调的，可把“闭区间”比较变成无符号整数比较，同时自然区分
//! `-0.0` 与 `+0.0`。`NaN` 没有全序位置，任何范围都不匹配。

use crate::SearchError;

/// 多字节数值的字节序。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Endian {
    /// 小端。
    Little,
    /// 大端。
    Big,
}

/// 数值查询的精确宽度与符号类别。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NumericKind {
    /// 无符号 8 位。
    U8,
    /// 无符号 16 位。
    U16,
    /// 无符号 32 位。
    U32,
    /// 无符号 64 位。
    U64,
    /// 有符号 8 位。
    I8,
    /// 有符号 16 位。
    I16,
    /// 有符号 32 位。
    I32,
    /// 有符号 64 位。
    I64,
    /// IEEE-754 单精度。
    F32,
    /// IEEE-754 双精度。
    F64,
}

/// 数值查询的一个边界值。
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum NumericValue {
    /// 无符号整数边界。
    Unsigned(u64),
    /// 有符号整数边界。
    Signed(i64),
    /// 浮点边界。
    Float(f64),
}

impl NumericKind {
    /// 宽度（字节）。
    pub const fn width(self) -> usize {
        match self {
            Self::U8 | Self::I8 => 1,
            Self::U16 | Self::I16 => 2,
            Self::U32 | Self::I32 | Self::F32 => 4,
            Self::U64 | Self::I64 | Self::F64 => 8,
        }
    }

    /// 是否无符号整数。
    pub const fn is_unsigned(self) -> bool {
        matches!(self, Self::U8 | Self::U16 | Self::U32 | Self::U64)
    }

    /// 是否有符号整数。
    pub const fn is_signed(self) -> bool {
        matches!(self, Self::I8 | Self::I16 | Self::I32 | Self::I64)
    }

    /// 是否浮点。
    pub const fn is_float(self) -> bool {
        matches!(self, Self::F32 | Self::F64)
    }

    fn unsigned_max(self) -> u64 {
        match self {
            Self::U8 => u64::from(u8::MAX),
            Self::U16 => u64::from(u16::MAX),
            Self::U32 => u64::from(u32::MAX),
            Self::U64 => u64::MAX,
            _ => 0,
        }
    }

    fn signed_min(self) -> i64 {
        match self {
            Self::I8 => i64::from(i8::MIN),
            Self::I16 => i64::from(i16::MIN),
            Self::I32 => i64::from(i32::MIN),
            Self::I64 => i64::MIN,
            _ => 0,
        }
    }

    fn signed_max(self) -> i64 {
        match self {
            Self::I8 => i64::from(i8::MAX),
            Self::I16 => i64::from(i16::MAX),
            Self::I32 => i64::from(i32::MAX),
            Self::I64 => i64::MAX,
            _ => 0,
        }
    }
}

/// 数值范围查询。
///
/// `min`/`max` 是闭区间；`min > max` 时构造器会自动交换两者（设计文档
/// F8 的“自动纠正并提示”行为，提示由 UI 层负责）。
#[derive(Clone, Debug, PartialEq)]
pub struct NumericQuery {
    /// 数值宽度与类别。
    pub kind: NumericKind,
    /// 字节序。
    pub endian: Endian,
    /// 区间下界。
    pub min: NumericValue,
    /// 区间上界。
    pub max: NumericValue,
    /// 可选绝对偏移对齐（例如 4 表示只检查偏移能被 4 整除的位置）。
    pub alignment: Option<usize>,
}

impl NumericQuery {
    /// 通用构造器；按 `kind` 校验边界类型与宽度，并在 `min > max` 时交换。
    pub fn new(
        kind: NumericKind,
        endian: Endian,
        min: NumericValue,
        max: NumericValue,
    ) -> Result<Self, SearchError> {
        let query = Self {
            kind,
            endian,
            min,
            max,
            alignment: None,
        };
        // 复用 compile 的校验逻辑；编译结果在真正搜索时会重新生成。
        query.compile()?;
        Ok(query)
    }

    /// 构造无符号整数范围。
    pub fn unsigned(
        kind: NumericKind,
        endian: Endian,
        min: u64,
        max: u64,
    ) -> Result<Self, SearchError> {
        Self::new(
            kind,
            endian,
            NumericValue::Unsigned(min),
            NumericValue::Unsigned(max),
        )
    }

    /// 构造有符号整数范围。
    pub fn signed(
        kind: NumericKind,
        endian: Endian,
        min: i64,
        max: i64,
    ) -> Result<Self, SearchError> {
        Self::new(
            kind,
            endian,
            NumericValue::Signed(min),
            NumericValue::Signed(max),
        )
    }

    /// 构造浮点范围。
    pub fn float(
        kind: NumericKind,
        endian: Endian,
        min: f64,
        max: f64,
    ) -> Result<Self, SearchError> {
        Self::new(
            kind,
            endian,
            NumericValue::Float(min),
            NumericValue::Float(max),
        )
    }

    /// 设置绝对偏移对齐约束。
    pub fn with_alignment(mut self, alignment: usize) -> Self {
        self.alignment = Some(alignment);
        self
    }

    /// 编译为扫描器。
    pub(crate) fn compile(&self) -> Result<CompiledNumeric, SearchError> {
        if self.kind.is_unsigned() {
            let (NumericValue::Unsigned(mut min), NumericValue::Unsigned(mut max)) =
                (self.min, self.max)
            else {
                return Err(SearchError::invalid_numeric(
                    "无符号整数查询的边界必须是 Unsigned",
                ));
            };
            let upper = self.kind.unsigned_max();
            if min > upper || max > upper {
                return Err(SearchError::invalid_numeric(format!(
                    "{:?} 的边界超出 {}",
                    self.kind, upper
                )));
            }
            if min > max {
                core::mem::swap(&mut min, &mut max);
            }
            let alignment = validate_alignment(self.alignment)?;
            Ok(CompiledNumeric::Unsigned {
                width: self.kind.width(),
                endian: self.endian,
                min,
                max,
                alignment,
            })
        } else if self.kind.is_signed() {
            let (NumericValue::Signed(mut min), NumericValue::Signed(mut max)) =
                (self.min, self.max)
            else {
                return Err(SearchError::invalid_numeric(
                    "有符号整数查询的边界必须是 Signed",
                ));
            };
            let lower = self.kind.signed_min();
            let upper = self.kind.signed_max();
            if min < lower || max > upper {
                return Err(SearchError::invalid_numeric(format!(
                    "{:?} 的边界超出 [{lower}, {upper}]",
                    self.kind
                )));
            }
            if min > max {
                core::mem::swap(&mut min, &mut max);
            }
            let alignment = validate_alignment(self.alignment)?;
            Ok(CompiledNumeric::Signed {
                width: self.kind.width(),
                endian: self.endian,
                min,
                max,
                alignment,
            })
        } else {
            let (NumericValue::Float(mut min), NumericValue::Float(mut max)) = (self.min, self.max)
            else {
                return Err(SearchError::invalid_numeric("浮点查询的边界必须是 Float"));
            };
            if min.is_nan() || max.is_nan() {
                return Err(SearchError::invalid_numeric("浮点边界不能是 NaN"));
            }
            if min > max {
                core::mem::swap(&mut min, &mut max);
            }
            let alignment = validate_alignment(self.alignment)?;
            Ok(CompiledNumeric::Float {
                width: self.kind.width(),
                endian: self.endian,
                min_key: total_key_f64(min),
                max_key: total_key_f64(max),
                alignment,
            })
        }
    }
}

fn validate_alignment(alignment: Option<usize>) -> Result<Option<usize>, SearchError> {
    match alignment {
        Some(0) => Err(SearchError::invalid_numeric("对齐值不能为 0")),
        other => Ok(other),
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum CompiledNumeric {
    Unsigned {
        width: usize,
        endian: Endian,
        min: u64,
        max: u64,
        alignment: Option<usize>,
    },
    Signed {
        width: usize,
        endian: Endian,
        min: i64,
        max: i64,
        alignment: Option<usize>,
    },
    Float {
        width: usize,
        endian: Endian,
        min_key: u64,
        max_key: u64,
        alignment: Option<usize>,
    },
}

impl CompiledNumeric {
    pub(crate) fn width(self) -> usize {
        match self {
            Self::Unsigned { width, .. }
            | Self::Signed { width, .. }
            | Self::Float { width, .. } => width,
        }
    }

    /// 检查 `bytes`（长度必须等于宽度）在绝对偏移 `offset` 处是否命中。
    pub(crate) fn matches(self, offset: u64, bytes: &[u8]) -> bool {
        let (width, alignment) = match self {
            Self::Unsigned {
                width, alignment, ..
            }
            | Self::Signed {
                width, alignment, ..
            }
            | Self::Float {
                width, alignment, ..
            } => (width, alignment),
        };
        if bytes.len() != width {
            return false;
        }
        if let Some(align) = alignment {
            if align == 0 || offset % align as u64 != 0 {
                return false;
            }
        }
        match self {
            Self::Unsigned {
                endian, min, max, ..
            } => {
                let value = read_uint(bytes, endian);
                value >= min && value <= max
            }
            Self::Signed {
                endian, min, max, ..
            } => {
                let value = sign_extend(read_uint(bytes, endian), width);
                value >= min && value <= max
            }
            Self::Float {
                endian,
                min_key,
                max_key,
                ..
            } => {
                if width == 4 {
                    let bits = read_uint(bytes, endian) as u32;
                    let value = f32::from_bits(bits);
                    if value.is_nan() {
                        return false;
                    }
                    // 统一到 f64 的 totalOrder 键空间，才能与 f64 边界比较。
                    let key = total_key_f64(f64::from(value));
                    key >= min_key && key <= max_key
                } else {
                    let bits = read_uint(bytes, endian);
                    let value = f64::from_bits(bits);
                    if value.is_nan() {
                        return false;
                    }
                    let key = total_key_f64(value);
                    key >= min_key && key <= max_key
                }
            }
        }
    }
}

fn read_uint(bytes: &[u8], endian: Endian) -> u64 {
    let mut value = 0u64;
    match endian {
        Endian::Big => {
            for &byte in bytes {
                value = (value << 8) | u64::from(byte);
            }
        }
        Endian::Little => {
            for &byte in bytes.iter().rev() {
                value = (value << 8) | u64::from(byte);
            }
        }
    }
    value
}

fn sign_extend(value: u64, width: usize) -> i64 {
    if width >= 8 {
        value as i64
    } else {
        let shift = 64 - width * 8;
        ((value << shift) as i64) >> shift
    }
}

/// IEEE-754 totalOrder 键（双精度）。
fn total_key_f64(value: f64) -> u64 {
    let bits = value.to_bits();
    if value.is_sign_negative() {
        !bits
    } else {
        bits | 0x8000_0000
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsigned_ranges_are_endian_aware() {
        let le = NumericQuery::unsigned(NumericKind::U16, Endian::Little, 0x1234, 0x1234)
            .expect("合法")
            .compile()
            .expect("编译");
        assert!(le.matches(0, &[0x34, 0x12]));
        assert!(!le.matches(0, &[0x12, 0x34]));

        let be = NumericQuery::unsigned(NumericKind::U32, Endian::Big, 0x0102_0304, 0x0102_0304)
            .expect("合法")
            .compile()
            .expect("编译");
        assert!(be.matches(0, &[0x01, 0x02, 0x03, 0x04]));
        assert!(!be.matches(0, &[0x04, 0x03, 0x02, 0x01]));
    }

    #[test]
    fn signed_widths_sign_extend() {
        let query = NumericQuery::signed(NumericKind::I8, Endian::Little, -1, -1)
            .expect("合法")
            .compile()
            .expect("编译");
        assert!(query.matches(0, &[0xFF]));
        assert!(!query.matches(0, &[0x01]));

        let wide = NumericQuery::signed(NumericKind::I16, Endian::Big, -2, -2)
            .expect("合法")
            .compile()
            .expect("编译");
        assert!(wide.matches(0, &[0xFF, 0xFE]));
        assert!(!wide.matches(0, &[0x00, 0x02]));
    }

    #[test]
    fn float_zero_signs_are_distinct() {
        let positive = NumericQuery::float(NumericKind::F64, Endian::Little, 0.0, 0.0)
            .expect("合法")
            .compile()
            .expect("编译");
        assert!(positive.matches(0, &0.0f64.to_le_bytes()));
        assert!(!positive.matches(0, &(-0.0f64).to_le_bytes()));

        let negative = NumericQuery::float(NumericKind::F64, Endian::Little, -0.0, -0.0)
            .expect("合法")
            .compile()
            .expect("编译");
        assert!(negative.matches(0, &(-0.0f64).to_le_bytes()));
        assert!(!negative.matches(0, &0.0f64.to_le_bytes()));
    }

    #[test]
    fn nan_never_matches() {
        let query = NumericQuery::float(NumericKind::F32, Endian::Big, f64::MIN, f64::MAX)
            .expect("合法")
            .compile()
            .expect("编译");
        assert!(!query.matches(0, &f32::NAN.to_be_bytes()));
        let negative_nan = -f32::NAN;
        assert!(!query.matches(0, &negative_nan.to_be_bytes()));
        assert!(query.matches(0, &1.5f32.to_be_bytes()));
    }

    #[test]
    fn invalid_bounds_and_alignment_are_rejected() {
        assert!(NumericQuery::unsigned(NumericKind::U8, Endian::Little, 0, 256).is_err());
        assert!(NumericQuery::signed(NumericKind::I8, Endian::Little, -129, 0).is_err());
        assert!(NumericQuery::float(NumericKind::F32, Endian::Little, f64::NAN, 0.0).is_err());
        assert!(
            NumericQuery::unsigned(NumericKind::U16, Endian::Little, 0, 1)
                .expect("合法")
                .with_alignment(0)
                .compile()
                .is_err()
        );
        // 边界类型与 kind 不匹配。
        assert!(
            NumericQuery::new(
                NumericKind::U8,
                Endian::Little,
                NumericValue::Signed(1),
                NumericValue::Signed(2),
            )
            .is_err()
        );
    }

    #[test]
    fn min_greater_than_max_is_swapped() {
        let query = NumericQuery::unsigned(NumericKind::U8, Endian::Little, 9, 3)
            .expect("应自动交换")
            .compile()
            .expect("编译");
        assert!(query.matches(0, &[5]));
        assert!(!query.matches(0, &[2]));
        assert!(!query.matches(0, &[10]));
    }

    #[test]
    fn float_ranges_follow_numeric_order() {
        let query = NumericQuery::float(NumericKind::F64, Endian::Little, -2.0, -1.0)
            .expect("合法")
            .compile()
            .expect("编译");
        assert!(query.matches(0, &(-1.5f64).to_le_bytes()));
        assert!(!query.matches(0, &(-0.5f64).to_le_bytes()));
        assert!(!query.matches(0, &(-2.5f64).to_le_bytes()));

        // F32 的边界与数据统一提升到 f64 totalOrder 键空间后仍有序。
        let f32_query = NumericQuery::float(NumericKind::F32, Endian::Big, 1.0, 2.0)
            .expect("合法")
            .compile()
            .expect("编译");
        assert!(f32_query.matches(0, &1.5f32.to_be_bytes()));
        assert!(!f32_query.matches(0, &2.5f32.to_be_bytes()));
    }

    #[test]
    fn alignment_filters_absolute_offsets() {
        let query = NumericQuery::unsigned(NumericKind::U16, Endian::Little, 1, 10)
            .expect("合法")
            .with_alignment(4)
            .compile()
            .expect("编译");
        assert!(query.matches(4, &[2, 0]));
        assert!(!query.matches(2, &[2, 0]));
    }
}
