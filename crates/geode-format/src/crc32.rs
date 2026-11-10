//! CRC-32/IEEE 802.3 查表实现。
//!
//! DESIGN: 23 §3.6 / §3.8
//!
//! reflected polynomial `0xEDB88320`，init/xorout 均为 `0xFFFF_FFFF`，
//! 与 zlib/zip 的 `crc32()` 等价。**不引入第三方依赖**：表是编译期常量。

/// CRC-32 查表（256 项，编译期生成）。
const TABLE: [u32; 256] = build_table();

/// 编译期构造 CRC-32 表。
const fn build_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0usize;
    while i < 256 {
        let mut crc = i as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
            bit += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
}

/// 计算 CRC-32/IEEE。
///
/// 调用方无需自行做 init/xorout：本函数已经包含 `0xFFFF_FFFF` 的初值与终值异或。
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        let index = ((crc ^ u32::from(byte)) & 0xFF) as usize;
        crc = (crc >> 8) ^ TABLE[index];
    }
    crc ^ 0xFFFF_FFFF
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vector_123456789() {
        // CRC-32/ISO-HDLC 的标准校验值。
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn empty_is_zero() {
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn single_zero_byte() {
        assert_eq!(crc32(&[0]), 0xD202_EF8D);
    }
}
