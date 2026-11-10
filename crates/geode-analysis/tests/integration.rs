//! MemoryProvider 集成测试：验证真实数据源上的分块路径，而不是只测纯函数。

use geode_analysis::{
    Analyzer, CancelToken, EntropyProfile, HashAlgo, MAX_SUMMARY_BUCKETS, hash_hex, hash_stream,
};
use geode_provider::MemoryProvider;

/// 构造带可打印串、低熵段与高熵段的测试数据。
fn sample_data(len: usize) -> Vec<u8> {
    let mut data = Vec::with_capacity(len);
    data.extend_from_slice(b"\x00\x01M2 T2.7 integration\x00");
    let mut state = 0x1234_5678u32;
    while data.len() < len {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        data.push((state >> 16) as u8);
    }
    data.truncate(len);
    data
}

#[test]
fn hash_stream_matches_one_shot_on_memory_provider() {
    let data = sample_data(3 * 1024 * 1024 + 12345);
    let provider = MemoryProvider::new("hash-integration", data.clone());
    let cancel = CancelToken::new();

    for algo in HashAlgo::ALL {
        let outcome = hash_stream(&provider, algo, &cancel).expect("内存源不应失败");
        assert_eq!(outcome.bytes, data.len() as u64);
        assert_eq!(outcome.hex, hash_hex(algo, &data), "{}", algo.name());
    }
}

#[test]
fn entropy_scan_and_summary_are_bounded() {
    let mut data = vec![b'A'; 128 * 1024];
    data.extend(sample_data(256 * 1024));
    let provider = MemoryProvider::new("entropy-integration", data);
    let profile =
        EntropyProfile::scan(&provider, 512, 256, &CancelToken::new()).expect("扫描应成功");

    assert_eq!(profile.window, 512);
    assert_eq!(profile.step, 256);
    assert!(!profile.points.is_empty());
    assert!(
        profile
            .points
            .iter()
            .all(|point| (0.0..=8.0).contains(&point.entropy))
    );
    assert!(profile.points.last().unwrap().entropy > 7.0);

    let summary = profile.summary(64);
    assert!(!summary.is_empty());
    assert!(summary.len() <= 64);
    assert!(summary.len() <= MAX_SUMMARY_BUCKETS);
    assert!(summary.iter().all(|bucket| bucket.min <= bucket.max));
}

#[test]
fn analyzer_returns_consistent_multi_analyzer_report() {
    let data = sample_data(2 * 1024 * 1024 + 7);
    let provider = MemoryProvider::new("analyzer-integration", data.clone());
    let report = Analyzer::default().run(&provider, &CancelToken::new());

    assert!(report.is_complete());
    assert_eq!(report.bytes_read, data.len() as u64);
    assert_eq!(report.holes, 0);
    assert_eq!(report.digests.len(), 2);
    assert_eq!(report.digests[0].hex, hash_hex(HashAlgo::Crc32Ieee, &data));
    assert_eq!(report.digests[1].hex, hash_hex(HashAlgo::Blake3, &data));

    let histogram = report.histogram.as_ref().unwrap();
    assert_eq!(histogram.total(), data.len() as u64);
    assert!(histogram.top_n(4).len() <= 4);

    let entropy = report.entropy.as_ref().unwrap();
    assert!(entropy.global > 0.0);
    assert!(entropy.buckets.len() <= MAX_SUMMARY_BUCKETS);

    let strings = report.strings.as_ref().unwrap();
    assert!(
        strings
            .entries
            .iter()
            .any(|entry| entry.text == "M2 T2.7 integration")
    );
}
