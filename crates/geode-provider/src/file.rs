//! 本地文件数据源——**`pread` 路径**，不做整读、默认不 mmap。
//!
//! DESIGN: 06 §4.1（mmap 仅用于"只读且大小稳定"）、04 §4.1
//!
//! M0 只实现 `pread`：它是唯一在任何文件大小下都安全的路。
//! mmap 会在 M1 以 `MappedProvider` 形式加入，且**仅当**文件只读、
//! 大小在打开后不再变化时启用（SIGBUS 成因链见 04 附录 A）。

use crate::{Caps, Provider, ProviderError, ReadReport, SizeInfo};
use geode_core::{Offset, ProviderId};
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

/// 本地文件（只读）。
#[derive(Debug)]
pub struct FileProvider {
    id: ProviderId,
    path: PathBuf,
    file: File,
    len: u64,
    writable: bool,
}

impl FileProvider {
    /// 以只读方式打开。
    ///
    /// 打开时**只 stat 不读**——4 GiB 文件的打开耗时必须是微秒级（18 §10 M0 验收①）。
    pub fn open(path: impl AsRef<Path>) -> Result<Self, ProviderError> {
        let path = path.as_ref().to_path_buf();
        let file = File::open(&path).map_err(|e| match e.kind() {
            io::ErrorKind::PermissionDenied => ProviderError::Permission {
                detail: format!("{path:?}: {e}"),
            },
            _ => ProviderError::Io(e),
        })?;
        let meta = file.metadata()?;
        if meta.is_dir() {
            return Err(ProviderError::BadFormat {
                detail: format!("{path:?} 是目录，不是文件"),
            });
        }
        let len = meta.len();
        let id = ProviderId::new(hash_path(&path));
        Ok(Self {
            id,
            path,
            file,
            len,
            writable: false,
        })
    }

    /// 以读写方式打开（覆盖编辑 / 就地保存用，见 04 §4.6）。
    ///
    /// 注意：**可写句柄绝不 mmap**（04 附录 A：Windows 映射期间不能 truncate，
    /// 且外部写入会让映射内容与页缓存不一致）。
    pub fn open_rw(path: impl AsRef<Path>) -> Result<Self, ProviderError> {
        let path = path.as_ref().to_path_buf();
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|e| match e.kind() {
                io::ErrorKind::PermissionDenied => ProviderError::Permission {
                    detail: format!("{path:?}: {e}（需要写权限）"),
                },
                _ => ProviderError::Io(e),
            })?;
        let meta = file.metadata()?;
        if meta.is_dir() {
            return Err(ProviderError::BadFormat {
                detail: format!("{path:?} 是目录，不是文件"),
            });
        }
        let len = meta.len();
        let id = ProviderId::new(hash_path(&path));
        Ok(Self {
            id,
            path,
            file,
            len,
            writable: true,
        })
    }

    /// 是否以读写方式打开。
    pub fn is_writable(&self) -> bool {
        self.writable
    }

    /// 在 `at` 处写入（`pwrite`，不移动文件游标，见 04 §4.1）。
    ///
    /// 返回实际写入字节数；短写是合法的（00 §5.1 铁律），调用方必须循环直到写完。
    pub fn write_at(&self, at: Offset, buf: &[u8]) -> Result<usize, ProviderError> {
        if !self.writable {
            return Err(ProviderError::Permission {
                detail: format!("{:?} 以只读方式打开", self.path),
            });
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileExt;
            Ok(self.file.write_at(buf, at.get())?)
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::FileExt;
            Ok(self.file.seek_write(buf, at.get())?)
        }
    }

    /// 循环写直到写完（短写重试，00 §5.1 铁律）。
    ///
    /// `write_at` 允许短写，调用方不该假设一次写完；返回 0 且仍未写完视为错误，
    /// 否则会变成死循环。
    pub fn write_all_at(&self, at: Offset, buf: &[u8]) -> Result<(), ProviderError> {
        let mut done = 0usize;
        while done < buf.len() {
            let here = at.checked_add(done as u64).ok_or_else(|| {
                ProviderError::Io(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "写入偏移溢出 u64",
                ))
            })?;
            let n = self.write_at(here, &buf[done..])?;
            if n == 0 {
                return Err(ProviderError::Io(io::Error::new(
                    io::ErrorKind::WriteZero,
                    format!("{at:?}+{done} 处写入返回 0"),
                )));
            }
            done += n;
        }
        Ok(())
    }

    /// 把文件内容与元数据刷到盘（`fsync`）。保存事务的第二步（04 §4.6）。
    pub fn sync_all(&self) -> Result<(), ProviderError> {
        self.file.sync_all()?;
        Ok(())
    }

    /// 路径。
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 重新读取长度。文件可能被外部截断/追加（06 §3.4）。
    pub fn refresh_len(&mut self) -> Result<u64, ProviderError> {
        self.len = self.file.metadata()?.len();
        Ok(self.len)
    }
}

fn hash_path(path: &Path) -> u32 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    path.hash(&mut h);
    (h.finish() & 0xFFFF_FFFF) as u32
}

impl Provider for FileProvider {
    fn id(&self) -> ProviderId {
        self.id
    }

    fn display_name(&self) -> String {
        format!("文件: {}", self.path.display())
    }

    fn caps(&self) -> Caps {
        if self.writable {
            Caps::READ_WRITE_STABLE
        } else {
            Caps::READ_ONLY_STABLE
        }
    }

    fn size(&self) -> SizeInfo {
        SizeInfo::Known(self.len)
    }

    fn read_range(&self, at: Offset, buf: &mut [u8]) -> Result<ReadReport, ProviderError> {
        let mut done = 0usize;
        // 越界不报错：裁剪到文件末尾，返回短读。
        while done < buf.len() {
            let pos = match at.checked_add(done as u64) {
                Some(p) if p.get() < self.len => p,
                _ => break,
            };
            let want =
                (buf.len() - done).min(usize::try_from(self.len - pos.get()).unwrap_or(usize::MAX));
            let n = pread(&self.file, &mut buf[done..done + want], pos.get())?;
            if n == 0 {
                break; // 并发截断：短读合法
            }
            done += n;
        }
        Ok(ReadReport {
            filled: done as u64,
            holes: 0,
            eof_hit: done < buf.len(),
        })
    }

    fn readable_ranges(&self) -> Option<Vec<geode_core::ByteRange>> {
        Some(vec![geode_core::ByteRange::from_len(
            Offset::ZERO,
            self.len,
        )?])
    }
}

#[cfg(unix)]
fn pread(file: &File, buf: &mut [u8], at: u64) -> io::Result<usize> {
    use std::os::unix::fs::FileExt;
    file.read_at(buf, at)
}

#[cfg(windows)]
fn pread(file: &File, buf: &mut [u8], at: u64) -> io::Result<usize> {
    use std::os::windows::fs::FileExt;
    // Windows 的 seek_read 在部分句柄上会截断 >4GiB 的读，故按 1 MiB 分段由上层保证；
    // 这里直接用 seek_read，偏移类型本身是 u64。
    file.seek_read(buf, at)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_file(bytes: &[u8]) -> (tempdir::Dir, PathBuf) {
        let dir = tempdir::Dir::new();
        let p = dir.path().join("sample.bin");
        std::fs::File::create(&p).unwrap().write_all(bytes).unwrap();
        (dir, p)
    }

    /// 极简临时目录（不引入 dev-dependency）。
    mod tempdir {
        pub struct Dir(std::path::PathBuf);
        impl Dir {
            pub fn new() -> Self {
                let mut p = std::env::temp_dir();
                p.push(format!(
                    "geode-test-{}-{}",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_nanos()
                ));
                std::fs::create_dir_all(&p).unwrap();
                Self(p)
            }
            pub fn path(&self) -> &std::path::Path {
                &self.0
            }
        }
        impl Drop for Dir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }

    #[test]
    fn reads_across_the_file() {
        let bytes: Vec<u8> = (0..=255u8).collect();
        let (_d, p) = temp_file(&bytes);
        let fp = FileProvider::open(&p).unwrap();
        assert_eq!(fp.size(), SizeInfo::Known(256));
        let mut buf = [0u8; 4];
        let r = fp.read_range(Offset::new(254), &mut buf).unwrap();
        assert_eq!((r.filled, r.holes, r.eof_hit), (2, 0, true));
        assert_eq!(&buf[..2], &[254, 255]);
    }

    #[test]
    fn out_of_bounds_is_short_read_not_panic() {
        let (_d, p) = temp_file(b"abc");
        let fp = FileProvider::open(&p).unwrap();
        let mut buf = [0u8; 8];
        let r = fp.read_range(Offset::new(1_000_000), &mut buf).unwrap();
        assert_eq!(r.filled, 0);
        assert!(r.eof_hit);
        let r2 = fp.read_range(Offset::new(u64::MAX - 1), &mut buf).unwrap();
        assert_eq!(r2.filled, 0);
    }

    #[test]
    fn empty_file_opens() {
        let (_d, p) = temp_file(b"");
        let fp = FileProvider::open(&p).unwrap();
        assert_eq!(fp.size(), SizeInfo::Known(0));
        assert!(fp.caps().sized);
        assert!(!fp.caps().writable);
    }

    #[test]
    fn directory_is_rejected() {
        let d = std::env::temp_dir();
        assert!(FileProvider::open(d).is_err());
    }
}
