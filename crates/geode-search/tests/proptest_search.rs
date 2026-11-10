//! proptest 属性：精确 needle 与朴素 `windows()` 对拍；任意模式搜索不 panic。

use geode_provider::MemoryProvider;
use geode_search::{
    Endian, HexPattern, NumericKind, NumericQuery, RegexQuery, SearchOptions, SearchQuery,
    TextEncoding, TextQuery, search,
};
use proptest::prelude::*;

fn arbitrary_text() -> impl Strategy<Value = String> {
    prop::collection::vec(any::<char>(), 0..40).prop_map(|chars| chars.into_iter().collect())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// 任意精确 needle 在任意 haystack 中的命中位置必须与朴素窗口扫描一致，
    /// 且不受块大小影响（覆盖跨块重叠逻辑）。
    #[test]
    fn exact_needle_matches_naive_windows(
        haystack in prop::collection::vec(any::<u8>(), 0..256),
        needle in prop::collection::vec(any::<u8>(), 1..32),
        chunk in 1usize..64,
    ) {
        let hex: String = needle.iter().map(|byte| format!("{byte:02x}")).collect();
        let pattern = HexPattern::parse(&hex).expect("精确字节模式必然合法");
        let provider = MemoryProvider::new("prop", haystack.clone());
        let opts = SearchOptions::default().with_chunk_size(chunk);
        let outcome = search(&provider, &SearchQuery::Hex(pattern), &opts, &|| false)
            .expect("搜索不应失败");

        let expected: Vec<(u64, u64)> = haystack
            .windows(needle.len())
            .enumerate()
            .filter(|(_, window)| *window == needle.as_slice())
            .map(|(start, _)| (start as u64, (start + needle.len()) as u64))
            .collect();
        let got: Vec<(u64, u64)> = outcome
            .hits
            .iter()
            .map(|range| (range.start().get(), range.end().get()))
            .collect();
        prop_assert_eq!(got, expected);
        prop_assert!(!outcome.truncated);
        prop_assert!(!outcome.cancelled);
    }

    /// 任意字符串/字节序列下，四种查询构造与搜索都不得 panic。
    #[test]
    fn arbitrary_patterns_never_panic(
        pattern in arbitrary_text(),
        haystack in prop::collection::vec(any::<u8>(), 0..128),
        chunk in 1usize..32,
    ) {
        let provider = MemoryProvider::new("prop-panic", haystack);
        let opts = SearchOptions::default().with_chunk_size(chunk);

        if let Ok(hex) = HexPattern::parse(&pattern) {
            let _ = search(&provider, &SearchQuery::Hex(hex), &opts, &|| false);
        }
        let _ = search(
            &provider,
            &SearchQuery::Regex(RegexQuery::new(pattern.clone())),
            &opts,
            &|| false,
        );
        if !pattern.is_empty() {
            let _ = search(
                &provider,
                &SearchQuery::Text(TextQuery::new(pattern, TextEncoding::Utf8)),
                &opts,
                &|| false,
            );
        }
        let numeric = NumericQuery::unsigned(NumericKind::U16, Endian::Little, 0, 100)
            .expect("固定查询合法");
        let _ = search(&provider, &SearchQuery::Numeric(numeric), &opts, &|| false);
        prop_assert!(true);
    }
}
