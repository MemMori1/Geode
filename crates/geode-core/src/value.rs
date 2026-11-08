//! 解析出的标量值。
//!
//! DESIGN: 02 §7.2、10 §3.5（Inspector）、13 §3

use core::fmt;

/// 一个叶子值。**浮点按位保存**：字节即真理，不引入 NaN 相等性问题
/// （见 02 P2 设计原则：绝不隐式转换）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    /// 无符号整数（最多 128 位）。
    U(u128),
    /// 有符号整数（最多 128 位）。
    I(i128),
    /// IEEE-754 半精度，存原始 16 位。
    F16Bits(u16),
    /// IEEE-754 单精度，存原始 32 位。
    F32Bits(u32),
    /// IEEE-754 双精度，存原始 64 位。
    F64Bits(u64),
    /// 布尔（由任意非零判定，来源可回链）。
    Bool(bool),
    /// 单个字符。
    Char(char),
    /// 原始字节串（压缩、哈希、GUID 等）。
    Bytes(Vec<u8>),
    /// 文本（已按声明编码解码）。
    Text(String),
    /// 无值（仅占位的 padding 或未知区）。
    None,
}

impl Value {
    /// 若可解释为 `u64`，取出数值。
    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Value::U(v) => u64::try_from(*v).ok(),
            Value::I(v) => u64::try_from(*v).ok(),
            Value::Bool(b) => Some(u64::from(*b)),
            _ => None,
        }
    }

    /// 若为文本，取出内容。
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Text(s) => Some(s),
            _ => None,
        }
    }

    /// 类型名（BinSpec 的类型名优先，退化为 Rust 侧名字）。
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::U(_) => "uint",
            Value::I(_) => "int",
            Value::F16Bits(_) => "f16",
            Value::F32Bits(_) => "f32",
            Value::F64Bits(_) => "f64",
            Value::Bool(_) => "bool",
            Value::Char(_) => "char",
            Value::Bytes(_) => "bytes",
            Value::Text(_) => "str",
            Value::None => "void",
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::U(v) => write!(f, "{v}"),
            Value::I(v) => write!(f, "{v}"),
            Value::F16Bits(b) => write!(f, "{}", half_to_f32(*b)),
            Value::F32Bits(b) => write!(f, "{}", f32::from_bits(*b)),
            Value::F64Bits(b) => write!(f, "{}", f64::from_bits(*b)),
            Value::Bool(b) => write!(f, "{b}"),
            Value::Char(c) => write!(f, "{c}"),
            Value::Bytes(b) => {
                for byte in b.iter().take(32) {
                    write!(f, "{byte:02X}")?;
                }
                if b.len() > 32 {
                    write!(f, "…({} B)", b.len())?;
                }
                Ok(())
            }
            Value::Text(s) => write!(f, "{s}"),
            Value::None => f.write_str("-"),
        }
    }
}

/// IEEE-754 半精度 → 单精度（用于显示；不做舍入优化）。
fn half_to_f32(bits: u16) -> f32 {
    let sign = u32::from(bits >> 15) << 31;
    let exp = (bits >> 10) & 0x1F;
    let frac = u32::from(bits & 0x3FF);
    let out = match exp {
        0 if frac == 0 => sign,
        0 => {
            // 次正规数：归一化
            let mut e = -1i32;
            let mut f = frac;
            while f & 0x400 == 0 {
                f <<= 1;
                e -= 1;
            }
            let exp32 = (127 - 15 + e + 1) as u32;
            sign | (exp32 << 23) | ((f & 0x3FF) << 13)
        }
        0x1F => sign | 0x7F80_0000 | (frac << 13),
        _ => sign | ((u32::from(exp) + 127 - 15) << 23) | (frac << 13),
    };
    f32::from_bits(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_precision_matches_known_values() {
        assert_eq!(half_to_f32(0x3C00), 1.0);
        assert_eq!(half_to_f32(0xC000), -2.0);
        assert_eq!(half_to_f32(0x0000), 0.0);
        assert!(half_to_f32(0x7C00).is_infinite());
    }

    #[test]
    fn bytes_display_truncates() {
        let v = Value::Bytes(vec![0xAB; 40]);
        assert!(v.to_string().contains("(40 B)"));
    }
}
