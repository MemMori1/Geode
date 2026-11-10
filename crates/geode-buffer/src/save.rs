//! 保存事务：**临时文件 + fsync + 原子 rename**（04 §4.6）。
//!
//! 为什么不能"就地写回原文件"：写到一半断电/崩溃/磁盘满，用户就得到一个
//! 半新半旧的固件镜像——这比丢一次编辑严重得多。正确顺序：
//!
//! 1. 同目录建临时文件（**必须同目录**，跨文件系统 rename 不是原子的）；
//! 2. 全量写入 + `fsync(temp)`；
//! 3. 复制原文件权限位（Unix）；
//! 4. 目标已存在时先建 `.bak` 硬链接（O(1)、不拷贝数据，失败不致命）；
//! 5. `rename(temp, dest)`——原子替换；
//! 6. `fsync(dir)`——让目录项落盘（否则断电后 rename 可能丢）。
//!
//! 失败时临时文件必须删除；目标文件保持原样。取消是**协作式**的：调用方通过
//! [`AtomicBool`] 置位，保存端在每块边界检查，取消后删临时文件并返回
//! [`SaveError::Cancelled`]——绝不会碰目标文件。

use geode_core::Offset;
use geode_provider::{Provider, ProviderError};
use std::fmt;
use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// 默认拷贝块大小：1 MiB（与 06 §3.2 的页缓存预算同量级）。
const CHUNK: usize = 1 << 20;

/// 保存失败的原因。
#[derive(Debug)]
pub enum SaveError {
    /// 数据源读失败。
    Provider(ProviderError),
    /// 本地 IO 失败。
    Io(io::Error),
    /// 调用方在保存过程中请求取消（目标文件未被修改）。
    Cancelled,
}

impl fmt::Display for SaveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Provider(e) => write!(f, "读取数据源失败：{e}"),
            Self::Io(e) => write!(f, "写入失败：{e}"),
            Self::Cancelled => write!(f, "保存已取消"),
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
    /// 是否成功建立了 `.bak` 硬链接。
    ///
    /// 目标不存在、文件系统不支持硬链接（FAT/exFAT/部分网络盘）或 `.bak`
    /// 被占用时为 `false`——备份失败**不阻止保存**（04 §4.6）。
    pub backup: bool,
}

/// 保存进度（每写完一块 1 MiB 回调一次，末尾必有一次 `written == total`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SaveProgress {
    /// 已写入临时文件的字节数（单调不减）。
    pub written: u64,
    /// 预计总字节数；数据源长度未知时随写入推进上调。
    pub total: u64,
}

/// 把 `src` 的**全部内容**原子地保存到 `dest`。
///
/// `dest` 已存在时会被替换；其 Unix 权限位会被保留（新建文件用默认 umask）。
/// 已有目标在替换前会尝试建立 `<dest>.bak` 硬链接。
pub fn save_provider_atomic(src: &dyn Provider, dest: &Path) -> Result<SaveStats, SaveError> {
    save_provider_atomic_with(src, dest, &mut |_| {})
}

/// 同 [`save_provider_atomic`]，但每写完一块（1 MiB）回调一次进度。
///
/// 回调可用来更新进度条；它拿不到取消权（取消见
/// [`save_provider_atomic_with_cancel`]）。保存成功时，最后一次回调满足
/// `written == total`（空源回调一次 `{0, 0}`）。
pub fn save_provider_atomic_with(
    src: &dyn Provider,
    dest: &Path,
    progress: &mut dyn FnMut(SaveProgress),
) -> Result<SaveStats, SaveError> {
    save_provider_atomic_with_cancel(src, dest, &AtomicBool::new(false), progress)
}

/// 同 [`save_provider_atomic_with`]，并支持**协作取消**。
///
/// 保存端在每块边界（以及提交前）检查 `cancel`；一旦置位，删除临时文件并返回
/// [`SaveError::Cancelled`]。目标文件要么是完整的旧内容，要么是完整的新内容，
/// 不会出现半成品。取消检查用 `Relaxed` 即可：它只要求最终可见，不承担同步职责。
pub fn save_provider_atomic_with_cancel(
    src: &dyn Provider,
    dest: &Path,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(SaveProgress),
) -> Result<SaveStats, SaveError> {
    let dir = dest
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let replaced = dest.exists();

    let tmp = temp_path(dir);
    let result = write_then_rename(src, dest, dir, &tmp, replaced, cancel, progress);
    if result.is_err() {
        // 失败/取消必须清理，否则用户目录里会慢慢堆满 .part 文件。
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
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(SaveProgress),
) -> Result<SaveStats, SaveError> {
    if cancelled(cancel) {
        return Err(SaveError::Cancelled);
    }

    let mut file = File::create(tmp)?;
    let mut written = 0u64;
    // 长度未知时以已探测下界起步，写入过程中只增不减（保证进度不倒退）。
    let mut total = src.size().lower_bound();
    let mut buf = vec![0u8; CHUNK];

    loop {
        if cancelled(cancel) {
            return Err(SaveError::Cancelled);
        }

        // 先把 1 MiB 缓冲填满（Provider 允许短读），再整块写出，
        // 这样"每写完一块回调一次"的口径与 CHUNK 一致。
        let mut filled = 0usize;
        while filled < CHUNK {
            if cancelled(cancel) {
                return Err(SaveError::Cancelled);
            }
            let at = Offset::new(written + filled as u64);
            let report = src.read_range(at, &mut buf[filled..])?;
            if report.filled == 0 {
                break;
            }
            let room = CHUNK - filled;
            let n = usize::try_from(report.filled).unwrap_or(room).min(room);
            filled += n;
            if report.eof_hit {
                break;
            }
        }
        if filled == 0 {
            break;
        }

        file.write_all(&buf[..filled])?;
        written += filled as u64;
        total = total.max(written);
        progress(SaveProgress { written, total });
    }

    // 保证"最后一次回调 written == total"：源被截短或为空时补一次收尾回调。
    if written != total || written == 0 {
        progress(SaveProgress {
            written,
            total: written,
        });
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

    // 取消可能由进度回调在最后一块之后置位；提交前再检查一次。
    if cancelled(cancel) {
        return Err(SaveError::Cancelled);
    }

    // 4. 提交前给旧目标留一个 O(1) 的硬链接备份。失败（不支持硬链接、
    //    .bak 被占用等）只记 backup=false，绝不阻止保存。
    let backup = if replaced { create_backup(dest) } else { false };

    std::fs::rename(tmp, dest)?; // 5. 原子替换

    #[cfg(unix)]
    {
        // 6. 目录项落盘。某些文件系统（如部分网络盘）不支持，失败不算致命。
        if let Ok(d) = File::open(dir) {
            let _ = d.sync_all();
        }
    }

    Ok(SaveStats {
        bytes: written,
        replaced,
        backup,
    })
}

/// 在 `dest` 旁边建 `<dest>.bak` 硬链接；成功返回 `true`。
///
/// 旧 `.bak` 先删掉（只保留最近一份）。任何一步失败都只返回 `false`。
fn create_backup(dest: &Path) -> bool {
    let bak = dest.with_extension("bak");
    // 目标本身就叫 `*.bak` 时 with_extension 会指回自己：宁可没有备份，
    // 也不能把目标删掉。
    if bak == dest {
        return false;
    }
    match std::fs::remove_file(&bak) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        // 删除失败（例如 .bak 是目录）留给下面的 hard_link 报错。
        Err(_) => {}
    }
    std::fs::hard_link(dest, &bak).is_ok()
}

fn cancelled(cancel: &AtomicBool) -> bool {
    cancel.load(Ordering::Relaxed)
}

fn temp_path(dir: &Path) -> PathBuf {
    use std::sync::atomic::AtomicU64;
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
