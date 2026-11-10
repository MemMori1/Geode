//! 哈希：自实现 CRC-32（IEEE）、BLAKE3、FNV-1a 32/64。
//!
//! DESIGN: 10 §4.2；Cargo.toml 只允许 `blake3` 一个哈希依赖，因此 CRC-32 用
//! 编译期查表法自实现，FNV-1a 按规范十行实现。
//!
//! 所有 [`Hasher`] 都支持任意切片分块喂入；`finish` 返回**大端**摘要字节，
//! 与 `blake3sum`、常见 CRC 工具的输出顺序一致。`Crc32Hasher` 的 4 字节输出是
//! `u32::to_be_bytes()`，十六进制显示即 `CBF43926` 这类习惯写法。

use geode_core::Offset;
use geode_provider::Provider;

use crate::cancel::CancelToken;
use crate::error::AnalysisError;

/// 单次读盘的块大小：1 MiB（10 §4.1）。
const STREAM_CHUNK: usize = 1 << 20;

/// 支持的哈希算法。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HashAlgo {
    /// CRC-32/IEEE（poly 0xEDB88320，init/xorout 0xFFFFFFFF），用于 zip/gzip 对拍。
    Crc32Ieee,
    /// BLAKE3，用于文件指纹与后续增量哈希树。
    Blake3,
    /// FNV-1a 32 位，用于分块去重与缓存键。
    Fnv1a32,
    /// FNV-1a 64 位。
    Fnv1a64,
}

impl HashAlgo {
    /// 全部算法，顺序与默认组一致（CRC32 在前、BLAKE3 在后）。
    pub const ALL: [Self; 4] = [Self::Crc32Ieee, Self::Blake3, Self::Fnv1a32, Self::Fnv1a64];

    /// 稳定名字（用于报告与 UI）。
    pub const fn name(self) -> &'static str {
        match self {
            Self::Crc32Ieee => "crc32",
            Self::Blake3 => "blake3",
            Self::Fnv1a32 => "fnv1a32",
            Self::Fnv1a64 => "fnv1a64",
        }
    }

    /// 摘要字节数。
    pub const fn digest_len(self) -> usize {
        match self {
            Self::Crc32Ieee => 4,
            Self::Blake3 => 32,
            Self::Fnv1a32 => 4,
            Self::Fnv1a64 => 8,
        }
    }
}

/// 可增量喂入的哈希器。
///
/// `finish(self)` 消耗自身，避免"finish 后继续 update"的无效状态。
pub trait Hasher {
    /// 喂入一段字节。
    fn update(&mut self, data: &[u8]);

    /// 完成计算并返回大端摘要字节。
    fn finish(self) -> Vec<u8>;
}

/// CRC-32/IEEE 查表。
const fn build_crc32_table() -> [u32; 256] {
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

/// 编译期生成的 256 项查表。
static CRC32_TABLE: [u32; 256] = build_crc32_table();

/// CRC-32/IEEE 增量哈希器。
#[derive(Clone, Debug)]
pub struct Crc32Hasher {
    state: u32,
}

impl Crc32Hasher {
    /// 创建新哈希器（内部初值 0xFFFFFFFF）。
    pub const fn new() -> Self {
        Self { state: 0xFFFF_FFFF }
    }

    /// 一次性计算字节切片的 CRC-32。
    pub fn checksum(data: &[u8]) -> u32 {
        let mut hasher = Self::new();
        hasher.update(data);
        let bytes = hasher.finish();
        u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
    }
}

impl Default for Crc32Hasher {
    fn default() -> Self {
        Self::new()
    }
}

impl Hasher for Crc32Hasher {
    fn update(&mut self, data: &[u8]) {
        let mut state = self.state;
        for &byte in data {
            let index = ((state ^ u32::from(byte)) & 0xFF) as usize;
            state = (state >> 8) ^ CRC32_TABLE[index];
        }
        self.state = state;
    }

    fn finish(self) -> Vec<u8> {
        (self.state ^ 0xFFFF_FFFF).to_be_bytes().to_vec()
    }
}

/// BLAKE3 增量哈希器（薄封装 `blake3::Hasher`）。
#[derive(Clone, Debug, Default)]
pub struct Blake3Hasher {
    hasher: Box<blake3::Hasher>,
}

impl Blake3Hasher {
    /// 创建新哈希器。
    pub fn new() -> Self {
        Self {
            hasher: Box::new(blake3::Hasher::new()),
        }
    }
}

impl Hasher for Blake3Hasher {
    fn update(&mut self, data: &[u8]) {
        self.hasher.update(data);
    }

    fn finish(self) -> Vec<u8> {
        self.hasher.finalize().as_bytes().to_vec()
    }
}

/// FNV-1a 32 位增量哈希器。
#[derive(Clone, Debug)]
pub struct Fnv1a32Hasher {
    state: u32,
}

impl Fnv1a32Hasher {
    /// 创建新哈希器。
    pub const fn new() -> Self {
        Self { state: 0x811C_9DC5 }
    }
}

impl Default for Fnv1a32Hasher {
    fn default() -> Self {
        Self::new()
    }
}

impl Hasher for Fnv1a32Hasher {
    fn update(&mut self, data: &[u8]) {
        let mut state = self.state;
        for &byte in data {
            state ^= u32::from(byte);
            state = state.wrapping_mul(0x0100_0193);
        }
        self.state = state;
    }

    fn finish(self) -> Vec<u8> {
        self.state.to_be_bytes().to_vec()
    }
}

/// FNV-1a 64 位增量哈希器。
#[derive(Clone, Debug)]
pub struct Fnv1a64Hasher {
    state: u64,
}

impl Fnv1a64Hasher {
    /// 创建新哈希器。
    pub const fn new() -> Self {
        Self {
            state: 0xCBF2_9CE4_8422_2325,
        }
    }
}

impl Default for Fnv1a64Hasher {
    fn default() -> Self {
        Self::new()
    }
}

impl Hasher for Fnv1a64Hasher {
    fn update(&mut self, data: &[u8]) {
        let mut state = self.state;
        for &byte in data {
            state ^= u64::from(byte);
            state = state.wrapping_mul(0x0000_0100_0000_01B3);
        }
        self.state = state;
    }

    fn finish(self) -> Vec<u8> {
        self.state.to_be_bytes().to_vec()
    }
}

/// 运行期选择算法的哈希器。
///
/// 不用 `Box<dyn Hasher>` 是因为 [`Hasher::finish`] 按值消费自身，
/// 而 trait object 无法按值移动；枚举分派保持 `finish(self)` 的原样签名。
#[derive(Clone, Debug)]
pub enum AnyHasher {
    /// CRC-32/IEEE。
    Crc32(Crc32Hasher),
    /// BLAKE3。
    Blake3(Blake3Hasher),
    /// FNV-1a 32 位。
    Fnv1a32(Fnv1a32Hasher),
    /// FNV-1a 64 位。
    Fnv1a64(Fnv1a64Hasher),
}

impl AnyHasher {
    /// 按算法创建哈希器。
    pub fn new(algo: HashAlgo) -> Self {
        match algo {
            HashAlgo::Crc32Ieee => Self::Crc32(Crc32Hasher::new()),
            HashAlgo::Blake3 => Self::Blake3(Blake3Hasher::new()),
            HashAlgo::Fnv1a32 => Self::Fnv1a32(Fnv1a32Hasher::new()),
            HashAlgo::Fnv1a64 => Self::Fnv1a64(Fnv1a64Hasher::new()),
        }
    }
}

impl Hasher for AnyHasher {
    fn update(&mut self, data: &[u8]) {
        match self {
            Self::Crc32(hasher) => hasher.update(data),
            Self::Blake3(hasher) => hasher.update(data),
            Self::Fnv1a32(hasher) => hasher.update(data),
            Self::Fnv1a64(hasher) => hasher.update(data),
        }
    }

    fn finish(self) -> Vec<u8> {
        match self {
            Self::Crc32(hasher) => hasher.finish(),
            Self::Blake3(hasher) => hasher.finish(),
            Self::Fnv1a32(hasher) => hasher.finish(),
            Self::Fnv1a64(hasher) => hasher.finish(),
        }
    }
}

/// 按算法创建哈希器。
pub fn new_hasher(algo: HashAlgo) -> AnyHasher {
    AnyHasher::new(algo)
}

/// 一次性计算切片摘要（大端字节）。
pub fn hash_bytes(algo: HashAlgo, data: &[u8]) -> Vec<u8> {
    let mut hasher = new_hasher(algo);
    hasher.update(data);
    hasher.finish()
}

/// 一次性计算切片摘要的十六进制串（小写）。
pub fn hash_hex(algo: HashAlgo, data: &[u8]) -> String {
    hex_encode(&hash_bytes(algo, data))
}

/// 字节转小写十六进制，不带分隔符。
pub fn hex_encode(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(char::from(DIGITS[usize::from(byte >> 4)]));
        out.push(char::from(DIGITS[usize::from(byte & 0x0F)]));
    }
    out
}

/// `hash_stream` 的结果。`bytes` 是实际喂入哈希器的字节数。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HashOutcome {
    /// 小写十六进制摘要。
    pub hex: String,
    /// 实际处理的字节数（取消或短读时可能小于数据源长度）。
    pub bytes: u64,
}

/// 按 1 MiB 分块读 provider 并计算摘要。
///
/// 短读按 `ReadReport.filled` 截断，不 panic；读到 0 字节或 `eof_hit` 即停。
/// 取消返回 [`AnalysisError::Cancelled`]——部分摘要没有意义，不能伪装成完整结果。
pub fn hash_stream<P: Provider + ?Sized>(
    provider: &P,
    algo: HashAlgo,
    cancel: &CancelToken,
) -> Result<HashOutcome, AnalysisError> {
    if cancel.is_cancelled() {
        return Err(AnalysisError::Cancelled);
    }
    let mut hasher = new_hasher(algo);
    let size_info = provider.size();
    let size_known = size_info.is_known();
    let size = size_info.lower_bound();
    let mut buffer = vec![0u8; STREAM_CHUNK];
    let mut offset = 0u64;

    // 长度未知时按块探测直到 EOF/短读为 0；长度已知时只读声明范围。
    while !size_known || offset < size {
        if cancel.is_cancelled() {
            return Err(AnalysisError::Cancelled);
        }
        let want = if size_known {
            usize::try_from((size - offset).min(STREAM_CHUNK as u64)).unwrap_or(STREAM_CHUNK)
        } else {
            STREAM_CHUNK
        };
        let report = provider.read_range(Offset::new(offset), &mut buffer[..want])?;
        let filled = usize::try_from(report.filled)
            .unwrap_or(usize::MAX)
            .min(want);
        if filled == 0 {
            break;
        }
        hasher.update(&buffer[..filled]);
        offset += filled as u64;
        if report.eof_hit {
            break;
        }
    }

    Ok(HashOutcome {
        hex: hex_encode(&hasher.finish()),
        bytes: offset,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_matches_known_vector() {
        // 10 §9 的公开向量。
        assert_eq!(Crc32Hasher::checksum(b"123456789"), 0xCBF4_3926);
        assert_eq!(Crc32Hasher::checksum(b""), 0);
        assert_eq!(Crc32Hasher::checksum(b"a"), 0xE8B7_BE43);
    }

    #[test]
    fn blake3_empty_matches_known_digest() {
        // BLAKE3 官方空输入向量。
        assert_eq!(
            hash_hex(HashAlgo::Blake3, b""),
            "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
        );
        assert_eq!(hash_bytes(HashAlgo::Blake3, b"").len(), 32);
    }

    #[test]
    fn fnv1a_matches_reference_vectors() {
        assert_eq!(hash_hex(HashAlgo::Fnv1a32, b""), "811c9dc5");
        assert_eq!(hash_hex(HashAlgo::Fnv1a32, b"a"), "e40c292c");
        assert_eq!(hash_hex(HashAlgo::Fnv1a32, b"123456789"), "bb86b11c");
        assert_eq!(hash_hex(HashAlgo::Fnv1a64, b""), "cbf29ce484222325");
        assert_eq!(hash_hex(HashAlgo::Fnv1a64, b"a"), "af63dc4c8601ec8c");
        assert_eq!(
            hash_hex(HashAlgo::Fnv1a64, b"123456789"),
            "06d5573923c6cdfc"
        );
    }

    #[test]
    fn chunked_updates_equal_one_shot() {
        let data: Vec<u8> = (0..10_000u32)
            .map(|i| (i.wrapping_mul(37) % 251) as u8)
            .collect();
        for algo in HashAlgo::ALL {
            let one_shot = hash_bytes(algo, &data);
            let mut hasher = new_hasher(algo);
            for chunk in data.chunks(777) {
                hasher.update(chunk);
            }
            assert_eq!(hasher.finish(), one_shot, "{}", algo.name());
        }
    }

    #[test]
    fn hex_encoding_is_lowercase() {
        assert_eq!(hex_encode(&[0x00, 0x0F, 0xAB, 0xFF]), "000fabff");
    }
}
