//! 保存事务的测试：**失败时原文件必须完好**，成功时内容逐字节正确。
//!
//! DESIGN: 04 §4.6（临时文件 + fsync + 原子 rename）、18 §10.2 T1.1

use geode_buffer::save::{SaveError, read_all, save_provider_atomic};
use geode_core::{ByteRange, Offset, ProviderId};
use geode_provider::{Caps, MemoryProvider, Provider, ProviderError, ReadReport, SizeInfo};
use std::path::PathBuf;

fn tmpdir(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("geode-save-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

/// 永远读失败的数据源：用来证明"写一半失败不碰原文件"。
#[derive(Debug)]
struct Broken;

impl Provider for Broken {
    fn id(&self) -> ProviderId {
        ProviderId::new(7)
    }
    fn display_name(&self) -> String {
        "broken".into()
    }
    fn caps(&self) -> Caps {
        Caps::READ_ONLY_STABLE
    }
    fn size(&self) -> SizeInfo {
        SizeInfo::Known(1024)
    }
    fn read_range(&self, _at: Offset, _buf: &mut [u8]) -> Result<ReadReport, ProviderError> {
        Err(ProviderError::Gone {
            detail: "模拟目标消失".into(),
        })
    }
    fn readable_ranges(&self) -> Option<Vec<ByteRange>> {
        None
    }
}

#[test]
fn roundtrip_bytes_match() {
    let dir = tmpdir("roundtrip");
    let dest = dir.join("out.bin");
    let data: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
    let src = MemoryProvider::new("mem", data.clone());

    let stats = save_provider_atomic(&src, &dest).unwrap();
    assert_eq!(stats.bytes, data.len() as u64);
    assert!(!stats.replaced);
    assert_eq!(std::fs::read(&dest).unwrap(), data);
    // 目录里不应残留 .part
    let leftovers: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".part"))
        .collect();
    assert!(leftovers.is_empty(), "临时文件没有清理：{leftovers:?}");
}

#[cfg(unix)]
#[test]
fn replaces_existing_and_keeps_mode() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tmpdir("replace");
    let dest = dir.join("fw.bin");
    std::fs::write(&dest, b"OLD-CONTENT-0123456789").unwrap();
    std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o640)).unwrap();

    let src = MemoryProvider::new("mem", b"NEW".to_vec());
    let stats = save_provider_atomic(&src, &dest).unwrap();
    assert!(stats.replaced);
    assert_eq!(std::fs::read(&dest).unwrap(), b"NEW");
    let mode = std::fs::metadata(&dest).unwrap().permissions().mode() & 0o777;
    assert_eq!(
        mode, 0o640,
        "权限位必须保留（否则用户的可执行固件变成不可执行）"
    );
}

#[test]
fn failure_leaves_target_untouched() {
    let dir = tmpdir("failure");
    let dest = dir.join("keep.bin");
    std::fs::write(&dest, b"DO-NOT-TOUCH").unwrap();

    let err = save_provider_atomic(&Broken, &dest).unwrap_err();
    assert!(
        matches!(err, SaveError::Provider(_)),
        "应当是读失败，而不是写失败"
    );
    assert_eq!(std::fs::read(&dest).unwrap(), b"DO-NOT-TOUCH");
    let leftovers = std::fs::read_dir(&dir).unwrap().count();
    assert_eq!(
        leftovers, 1,
        "失败后只应剩下原文件，实际 {} 个条目",
        leftovers
    );
}

#[test]
fn read_all_streams_whole_source() {
    let data: Vec<u8> = (0..(3 * 1024 * 1024 + 17) as u32)
        .map(|i| (i % 257) as u8)
        .collect();
    let src = MemoryProvider::new("mem", data.clone());
    assert_eq!(read_all(&src).unwrap(), data);
    // 断言源确实跨了多个 1 MiB 块。
    assert!(data.len() > 3 << 20);
}
