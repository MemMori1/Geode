//! 64 位文件偏移。
//!
//! DESIGN: 00 §4.1、05 §4.2

use core::fmt;

/// 文件内字节偏移，语义上等于 `u64`，但**禁止隐式**与长度/索引混用。
///
/// 之所以不用裸 `u64`：`Offset` 与 `Len`（长度）在算术上不可互换
/// （`a + b` 表示"偏移加长度"，`a - b` 表示"两个偏移相减得长度"），
/// 混用是十六进制编辑器里最常见的一类 bug。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Offset(u64);

impl Offset {
    /// 偏移 0。
    pub const ZERO: Self = Self(0);

    /// 从裸 `u64` 构造。
    #[inline]
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    /// 取出裸 `u64`。
    #[inline]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// 向前进位，溢出返回 `None` 而不是回绕。
    #[inline]
    pub const fn checked_add(self, len: u64) -> Option<Self> {
        match self.0.checked_add(len) {
            Some(v) => Some(Self(v)),
            None => None,
        }
    }

    /// 向后退位，越过 0 返回 `None`。
    #[inline]
    pub const fn checked_sub(self, len: u64) -> Option<Self> {
        match self.0.checked_sub(len) {
            Some(v) => Some(Self(v)),
            None => None,
        }
    }

    /// 饱和加法：溢出处停在 `u64::MAX`。
    ///
    /// 用于滚动与布局计算——那里"到底了"是正常状态，不该产生错误分支。
    #[inline]
    pub const fn saturating_add(self, len: u64) -> Self {
        Self(self.0.saturating_add(len))
    }

    /// 饱和减法：越界处停在 0。
    #[inline]
    pub const fn saturating_sub(self, len: u64) -> Self {
        Self(self.0.saturating_sub(len))
    }

    /// 对齐到 `align` 的整数倍（向下）。`align` 为 0 时返回自身。
    #[inline]
    pub const fn align_down(self, align: u64) -> Self {
        if align == 0 {
            self
        } else {
            Self(self.0 - self.0 % align)
        }
    }

    /// 对齐到 `align` 的整数倍（向上），`align` 为 0 时返回自身。
    #[inline]
    pub const fn align_up(self, align: u64) -> Self {
        if align == 0 {
            return self;
        }
        let rem = self.0 % align;
        if rem == 0 {
            self
        } else {
            Self(self.0 + (align - rem))
        }
    }
}

impl fmt::Display for Offset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "0x{:X}", self.0)
    }
}

impl From<u64> for Offset {
    fn from(v: u64) -> Self {
        Self(v)
    }
}

#[cfg(feature = "serde")]
impl serde::Serialize for Offset {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_does_not_wrap() {
        assert_eq!(Offset::new(u64::MAX).checked_add(1), None);
        assert_eq!(
            Offset::new(4).checked_add(0x8000_0000),
            Some(Offset::new(0x8000_0004))
        );
    }

    #[test]
    fn sub_stops_at_zero() {
        assert_eq!(Offset::new(2).checked_sub(3), None);
        assert_eq!(Offset::new(2).saturating_sub(3), Offset::ZERO);
    }

    #[test]
    fn align_rounds_both_ways() {
        assert_eq!(Offset::new(0x1234).align_down(512), Offset::new(0x1200));
        assert_eq!(Offset::new(0x1234).align_up(512), Offset::new(0x1400));
        assert_eq!(Offset::new(0x1200).align_up(512), Offset::new(0x1200));
        assert_eq!(Offset::new(7).align_up(0), Offset::new(7));
    }

    #[test]
    fn display_is_hex() {
        assert_eq!(format!("{}", Offset::new(0xdead)), "0xDEAD");
    }
}
