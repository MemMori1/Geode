//! VM 的运行时值。
//!
//! DESIGN: 02-dsl-spec-draft.md §3（类型系统）、§7（执行模型）

use std::fmt;

/// BinSpec 的运行时值。
///
/// 刻意保持扁平：没有引用、没有闭包、没有堆对象——模板语言不该有这些。
/// 结构体/数组在 `Node` 树里表达，不在值里嵌套。
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// 无符号整数（u8~u128 在 VM 里统一按 u128 存，写出时按字段类型截断）。
    UInt(u128),
    /// 有符号整数。
    Int(i128),
    /// 浮点（f16/f32 在读入时已提升为 f64）。
    Float(f64),
    /// 布尔。
    Bool(bool),
    /// 字符串（UTF-8）。
    Str(String),
    /// 原始字节（`byte[N]`、`str` 的未解码视图）。
    Bytes(Vec<u8>),
    /// 空值（未初始化 / 不可解引用的指针）。
    None,
}

impl Value {
    /// 当作无符号整数读；不兼容返回 `None`。
    #[must_use]
    pub fn as_u128(&self) -> Option<u128> {
        match self {
            Self::UInt(v) => Some(*v),
            Self::Int(v) => u128::try_from(*v).ok(),
            Self::Bool(b) => Some(u128::from(*b)),
            _ => None,
        }
    }

    /// 当作有符号整数读。
    #[must_use]
    pub fn as_i128(&self) -> Option<i128> {
        match self {
            Self::UInt(v) => i128::try_from(*v).ok(),
            Self::Int(v) => Some(*v),
            Self::Bool(b) => Some(i128::from(*b)),
            _ => None,
        }
    }

    /// 当作 f64 读。
    #[must_use]
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Float(v) => Some(*v),
            Self::UInt(v) => Some(*v as f64),
            Self::Int(v) => Some(*v as f64),
            _ => None,
        }
    }

    /// 当作布尔读（0 为假，非 0 为真）。
    #[must_use]
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(b) => Some(*b),
            other => other.as_u128().map(|v| v != 0),
        }
    }

    /// 类型名（诊断与 `typeof` 用）。
    #[must_use]
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::UInt(_) => "uint",
            Self::Int(_) => "int",
            Self::Float(_) => "float",
            Self::Bool(_) => "bool",
            Self::Str(_) => "str",
            Self::Bytes(_) => "bytes",
            Self::None => "none",
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UInt(v) => write!(f, "{v}"),
            Self::Int(v) => write!(f, "{v}"),
            Self::Float(v) => write!(f, "{v}"),
            Self::Bool(v) => write!(f, "{v}"),
            Self::Str(v) => write!(f, "{v}"),
            Self::Bytes(v) => write!(f, "<{} bytes>", v.len()),
            Self::None => write!(f, "none"),
        }
    }
}
