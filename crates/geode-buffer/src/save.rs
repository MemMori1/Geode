//! 保存事务：**临时文件 + fsync + 原子 rename**（04 §4.6）。
//!
//! 为什么不能"就地写回原文件"：写到一半断电/崩溃/磁盘满，用户就得到一个
//! 半新半旧的固件镜像——这比丢一次编辑严重得多。正确顺序：
//!
//! 1. 同目录建临时文件（**必须同目录**，跨文件系统 rename 不是原子的）；
//! 2. 全量写入 + `fsync(temp)`；
//! 3. 复制原文件权限位（Unix）；
//! 4. `rename(temp, dest)`——原子替换；
//! 5. `fsync(dir)`——让目录项落盘（否则断电后 rename 可能丢）。
//!
//! 失败时临时文件必须删除；目标文件保持原样。

use geode_core::Offset;
use geode_provider::{Provider, ProviderError};
use std::fmt;
use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// 默认拷贝块大小：1 MiB（与 06 §3.2 的页缓存预算同量级）。
const CHUNK: usize = 1 << 20;

/// 保存失败的原因。
#[derive(Debug)]
pub enum SaveError {
    /// 数据源读失败。
    Provider(ProviderError),
    /// 本地 IO 失败。
    Io(io::Error),
}

impl fmt::Display for SaveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Provider(e) => write!(f, "读取数据源失败：{e}"),
            Self::Io(e) => write!(f, "写入失败：{e}"),
        }
    }
}

impl std::error::Error for SaveError {}

impl From<io::Error> for SaveError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<ProviderError> for SaveError {
    fn from(e: ProviderError) -> Self {
        Self::Provider(e)
    }
}

/// 保存统计。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SaveStats {
    /// 写入字节数。
    pub bytes: u64,
    /// 是否替换了已有文件。
    pub replaced: bool,
}

/// 把 `src` 的**全部内容**原子地保存到 `dest`。
///
/// `dest` 已存在时会被替换；其 Unix 权限位会被保留（新建文件用默认 umask）。
pub fn save_provider_atomic(src: &dyn Provider, dest: &Path) -> Result<SaveStats, SaveError> {
    let dir = dest
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let replaced = dest.exists();

    let tmp = temp_path(dir);
    let result = write_then_rename(src, dest, dir, &tmp, replaced);
    if result.is_err() {
        // 失败必须清理，否则用户目录里会慢慢堆满 .part 文件。
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

fn write_then_rename(
    src: &dyn Provider,
    dest: &Path,
    dir: &Path,
    tmp: &Path,
    replaced: bool,
) -> Result<SaveStats, SaveError> {
    let _ = replaced;
    let mut file = File::create(tmp)?;
    let mut written = 0u64;
    let mut buf = vec![0u8; CHUNK];

    loop {
        let report = src.read_range(Offset::new(written), &mut buf)?;
        if report.filled == 0 {
            break;
        }
        let n = usize::try_from(report.filled).unwrap_or(buf.len());
        file.write_all(&buf[..n])?;
        written += report.filled;
        if report.eof_hit {
            break;
        }
    }

    file.flush()?;
    file.sync_all()?; // 2. 内容落盘

    #[cfg(unix)]
    if replaced {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(dest) {
            let mode = meta.permissions().mode();
            let _ = std::fs::set_permissions(tmp, std::fs::Permissions::from_mode(mode));
        }
    }

    std::fs::rename(tmp, dest)?; // 4. 原子替换

    #[cfg(unix)]
    {
        // 5. 目录项落盘。某些文件系统（如部分网络盘）不支持，失败不算致命。
        if let Ok(d) = File::open(dir) {
            let _ = d.sync_all();
        }
    }

    Ok(SaveStats {
        bytes: written,
        replaced,
    })
}

fn temp_path(dir: &Path) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    dir.join(format!(".geode-save-{}-{seq}.part", std::process::id()))
}

/// 把数据源从头到尾读进内存（仅供小文件与测试；大文件请用流式保存）。
pub fn read_all(src: &dyn Provider) -> Result<Vec<u8>, SaveError> {
    let mut out = Vec::new();
    let mut buf = vec![0u8; CHUNK];
    loop {
        let at = Offset::new(out.len() as u64);
        let report = src.read_range(at, &mut buf)?;
        if report.filled == 0 {
            break;
        }
        out.extend_from_slice(&buf[..report.filled as usize]);
        if report.eof_hit {
            break;
        }
    }
    Ok(out)
}
