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
        })
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
        Caps::READ_ONLY_STABLE
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
