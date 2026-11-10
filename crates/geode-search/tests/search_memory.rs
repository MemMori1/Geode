//! 基于 `MemoryProvider` 的集成测试：跨块命中、截断、取消。

use std::sync::atomic::{AtomicUsize, Ordering};

use geode_provider::MemoryProvider;
use geode_search::{HexPattern, SearchOptions, SearchQuery, search};

fn memory(bytes: &[u8]) -> MemoryProvider {
    MemoryProvider::new("integration", bytes.to_vec())
}

fn starts(outcome: &geode_search::SearchOutcome) -> Vec<(u64, u64)> {
    outcome
        .hits
        .iter()
        .map(|range| (range.start().get(), range.end().get()))
        .collect()
}

#[test]
fn overlapping_needle_crosses_chunks_without_duplicates() {
    let provider = memory(b"abcabcabc");
    let query = SearchQuery::Hex(HexPattern::from_bytes(b"abcabc"));
    let expected = vec![(0, 6), (3, 9)];
    for chunk in 1..=12 {
        let opts = SearchOptions::default().with_chunk_size(chunk);
        let outcome = search(&provider, &query, &opts, &|| false).expect("搜索");
        assert_eq!(starts(&outcome), expected, "chunk={chunk}");
        assert!(!outcome.truncated, "chunk={chunk}");
        assert!(!outcome.cancelled, "chunk={chunk}");
    }
}

#[test]
fn truncation_stops_immediately_at_limit() {
    let data: Vec<u8> = std::iter::repeat_n(*b"AB", 512 * 1024).flatten().collect();
    let provider = memory(&data);
    let query = SearchQuery::Hex(HexPattern::from_bytes(b"AB"));
    let opts = SearchOptions::default()
        .with_chunk_size(16 * 1024)
        .with_max_results(5);
    let outcome = search(&provider, &query, &opts, &|| false).expect("搜索");
    assert_eq!(
        starts(&outcome),
        vec![(0, 2), (2, 4), (4, 6), (6, 8), (8, 10)]
    );
    assert!(outcome.truncated);
    assert!(!outcome.cancelled);
    // 达到上限后不应继续扫描后续块。
    assert_eq!(outcome.bytes_scanned, 16 * 1024);
}

#[test]
fn cancellation_keeps_already_found_hits() {
    let data: Vec<u8> = std::iter::repeat_n(*b"AB", 128 * 1024).flatten().collect();
    let provider = memory(&data);
    let query = SearchQuery::Hex(HexPattern::from_bytes(b"AB"));
    let opts = SearchOptions::default().with_chunk_size(8 * 1024);
    let calls = AtomicUsize::new(0);
    // 第 1 次是入口检查，第 2 次是首块开始；第 3 次（第二块开始）取消。
    let cancel = || calls.fetch_add(1, Ordering::SeqCst) >= 2;
    let outcome = search(&provider, &query, &opts, &cancel).expect("搜索");
    assert!(outcome.cancelled);
    assert!(!outcome.truncated);
    assert_eq!(outcome.hits.len(), 4096);
    assert_eq!(outcome.bytes_scanned, 8 * 1024);
    assert_eq!(starts(&outcome)[0], (0, 2));
    assert_eq!(starts(&outcome).last().copied(), Some((8190, 8192)));
}
