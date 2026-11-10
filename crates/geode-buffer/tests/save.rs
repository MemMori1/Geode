//! 保存事务的测试：**失败时原文件必须完好**，成功时内容逐字节正确。
//!
//! DESIGN: 04 §4.6（临时文件 + fsync + 原子 rename）、18 §10.2 T1.1

use geode_buffer::save::{
    SaveError, SaveProgress, read_all, save_provider_atomic, save_provider_atomic_with,
    save_provider_atomic_with_cancel,
};
use geode_core::{ByteRange, Offset, ProviderId};
use geode_provider::{Caps, MemoryProvider, Provider, ProviderError, ReadReport, SizeInfo};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

fn tmpdir(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("geode-save-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

/// 目录内所有条目名（排序后便于断言）。
fn entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
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
    assert!(!stats.backup, "首次保存没有旧目标，不应有备份");
    assert_eq!(std::fs::read(&dest).unwrap(), data);
    assert!(!dest.with_extension("bak").exists());
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
fn backup_holds_previous_target_bytes() {
    let dir = tmpdir("backup");
    let dest = dir.join("fw.bin");
    std::fs::write(&dest, b"OLD-FIRMWARE").unwrap();
    // 预置一个过期 .bak：保存时只保留最近一份，必须被旧目标替换。
    std::fs::write(dest.with_extension("bak"), b"STALE-BACKUP").unwrap();

    #[cfg(unix)]
    let old_ino = {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(&dest).unwrap().ino()
    };

    let src = MemoryProvider::new("mem", b"NEW-FIRMWARE".to_vec());
    let stats = save_provider_atomic(&src, &dest).unwrap();

    assert!(stats.replaced);
    assert!(stats.backup, "同目录应能建立硬链接");
    assert_eq!(std::fs::read(&dest).unwrap(), b"NEW-FIRMWARE");
    let bak = dest.with_extension("bak");
    assert_eq!(
        std::fs::read(&bak).unwrap(),
        b"OLD-FIRMWARE",
        ".bak 必须是旧目标的逐字节内容"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(
            std::fs::metadata(&bak).unwrap().ino(),
            old_ino,
            ".bak 应当是旧目标的硬链接（同 inode），而不是拷贝"
        );
    }
}

#[test]
fn backup_failure_does_not_block_save() {
    let dir = tmpdir("backup-fail");
    let dest = dir.join("fw.bin");
    std::fs::write(&dest, b"OLD").unwrap();
    // .bak 被目录占住：删不掉也建不了硬链接，但保存必须照常完成。
    std::fs::create_dir(dest.with_extension("bak")).unwrap();

    let src = MemoryProvider::new("mem", b"NEW".to_vec());
    let stats = save_provider_atomic(&src, &dest).unwrap();
    assert!(stats.replaced);
    assert!(!stats.backup, "建链失败应如实记录");
    assert_eq!(std::fs::read(&dest).unwrap(), b"NEW");
}

#[test]
fn cancel_removes_part_and_keeps_target() {
    let dir = tmpdir("cancel");
    let dest = dir.join("fw.bin");
    std::fs::write(&dest, b"ORIGINAL").unwrap();

    let data: Vec<u8> = (0..(3 * 1024 * 1024) as u32)
        .map(|i| (i % 253) as u8)
        .collect();
    let src = MemoryProvider::new("mem", data);
    let cancel = AtomicBool::new(false);
    let mut calls = 0usize;

    let err = save_provider_atomic_with_cancel(&src, &dest, &cancel, &mut |p| {
        calls += 1;
        // 第一块写完就请求取消：后续块不再写，提交也不会发生。
        if p.written >= 1 << 20 {
            cancel.store(true, Ordering::Relaxed);
        }
    })
    .unwrap_err();

    assert!(matches!(err, SaveError::Cancelled));
    assert!(calls >= 1, "取消前至少应报告过一块的进度");
    assert_eq!(
        std::fs::read(&dest).unwrap(),
        b"ORIGINAL",
        "取消后目标文件必须原封不动"
    );
    assert_eq!(
        entries(&dir),
        vec!["fw.bin".to_string()],
        "取消后不应残留 .part（也不应提前建 .bak）"
    );
}

#[test]
fn progress_is_monotonic_and_ends_at_total() {
    let dir = tmpdir("progress");
    let dest = dir.join("out.bin");
    let len = 3 * (1 << 20) + 17;
    let data: Vec<u8> = (0..len as u32).map(|i| (i % 251) as u8).collect();
    let src = MemoryProvider::new("mem", data);

    let mut samples: Vec<SaveProgress> = Vec::new();
    let stats = save_provider_atomic_with(&src, &dest, &mut |p| samples.push(p)).unwrap();

    assert_eq!(stats.bytes, len as u64);
    assert_eq!(std::fs::read(&dest).unwrap().len(), len);
    assert!(!samples.is_empty(), "进度回调至少要发生一次");
    for pair in samples.windows(2) {
        assert!(
            pair[1].written >= pair[0].written,
            "written 必须单调不减：{pair:?}"
        );
        assert!(
            pair[1].total >= pair[0].total,
            "total 必须单调不减：{pair:?}"
        );
    }
    for p in &samples {
        assert!(p.written <= p.total, "written 不得超过 total：{p:?}");
        assert_eq!(p.total, len as u64, "已知长度时 total 应保持稳定");
    }
    let last = samples.last().unwrap();
    assert_eq!(last.written, len as u64);
    assert_eq!(last.written, last.total, "末次回调必须 100%");
    // 1 MiB 一块：3 个整块 + 17 字节尾块 = 4 次。
    assert_eq!(samples.len(), 4, "应当每 1 MiB 回调一次：{samples:?}");
}

#[test]
fn progress_reports_empty_source() {
    let dir = tmpdir("progress-empty");
    let dest = dir.join("empty.bin");
    let src = MemoryProvider::new("empty", Vec::new());

    let mut samples: Vec<SaveProgress> = Vec::new();
    let stats = save_provider_atomic_with(&src, &dest, &mut |p| samples.push(p)).unwrap();

    assert_eq!(stats.bytes, 0);
    assert_eq!(std::fs::read(&dest).unwrap(), Vec::<u8>::new());
    assert_eq!(
        samples,
        vec![SaveProgress {
            written: 0,
            total: 0
        }],
        "空源也必须有一次 written == total 的收尾回调"
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
