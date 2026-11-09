//! T1.1：可写句柄、短写重试、caps 变化。DESIGN: 04 §4.1/§4.6、06 §3.1

use geode_core::Offset;
use geode_provider::{Caps, FileProvider, Provider};

#[test]
fn open_rw_reports_writable_caps() {
    let p = std::env::temp_dir().join("geode-rw-caps.bin");
    std::fs::write(&p, b"abcd").unwrap();
    let f = FileProvider::open_rw(&p).unwrap();
    assert!(f.is_writable());
    assert!(f.caps().writable);
    let ro = FileProvider::open(&p).unwrap();
    assert!(!ro.is_writable());
    assert_eq!(ro.caps().bits(), Caps::READ_ONLY_STABLE.bits());
    std::fs::remove_file(&p).ok();
}

#[test]
fn write_all_at_extends_and_reads_back() {
    let p = std::env::temp_dir().join("geode-rw-write.bin");
    std::fs::write(&p, b"0123456789").unwrap();
    let f = FileProvider::open_rw(&p).unwrap();
    // 跨越 EOF 写：稀疏扩展是合法的（04 附录 A 的稀疏空洞）。
    let at = Offset::new(1 << 20);
    let data = vec![0xA5u8; 64 * 1024];
    f.write_all_at(at, &data).unwrap();
    f.sync_all().unwrap();
    let back = f.read_exact_vec(at, data.len() as u64).unwrap().0;
    assert_eq!(back, data);
    assert_eq!(std::fs::metadata(&p).unwrap().len(), (1 << 20) + 64 * 1024);
    std::fs::remove_file(&p).ok();
}

#[test]
fn read_only_handle_refuses_write() {
    let p = std::env::temp_dir().join("geode-ro-write.bin");
    std::fs::write(&p, b"x").unwrap();
    let f = FileProvider::open(&p).unwrap();
    assert!(f.write_at(Offset::ZERO, b"y").is_err());
    std::fs::remove_file(&p).ok();
}
