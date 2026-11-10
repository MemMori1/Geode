//! 空洞语义集成测试：可读区间之外的数据不得被搜索，也不得把补零当数据。

use core::fmt;
use std::cmp::min;

use geode_core::{ByteRange, Offset, ProviderId};
use geode_provider::{Caps, Provider, ReadReport, SizeInfo};
use geode_search::{HexPattern, SearchOptions, SearchQuery, search};

#[derive(Debug)]
struct HolesProvider {
    bytes: Vec<u8>,
    readable: Vec<ByteRange>,
}

impl HolesProvider {
    fn new(bytes: Vec<u8>, readable: Vec<ByteRange>) -> Self {
        Self { bytes, readable }
    }

    fn is_readable(&self, offset: usize) -> bool {
        self.readable.iter().any(|range| {
            let start = usize::try_from(range.start().get()).unwrap_or(usize::MAX);
            let end = usize::try_from(range.end().get()).unwrap_or(usize::MAX);
            offset >= start && offset < end
        })
    }
}

impl Provider for HolesProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new(0x484F_4C45)
    }

    fn display_name(&self) -> String {
        "holes-test".to_string()
    }

    fn caps(&self) -> Caps {
        Caps::READ_ONLY_STABLE
    }

    fn size(&self) -> SizeInfo {
        SizeInfo::Known(self.bytes.len() as u64)
    }

    fn read_range(
        &self,
        at: Offset,
        buf: &mut [u8],
    ) -> Result<ReadReport, geode_provider::ProviderError> {
        let start = usize::try_from(at.get()).unwrap_or(usize::MAX);
        if start >= self.bytes.len() {
            return Ok(ReadReport {
                filled: 0,
                holes: 0,
                eof_hit: true,
            });
        }
        let count = min(buf.len(), self.bytes.len() - start);
        let mut holes = 0u64;
        for (index, slot) in buf.iter_mut().take(count).enumerate() {
            let offset = start + index;
            if self.is_readable(offset) {
                *slot = self.bytes[offset];
            } else {
                *slot = 0;
                holes += 1;
            }
        }
        Ok(ReadReport {
            filled: count as u64,
            holes,
            eof_hit: count < buf.len(),
        })
    }

    fn readable_ranges(&self) -> Option<Vec<ByteRange>> {
        Some(self.readable.clone())
    }
}

impl fmt::Display for HolesProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("holes-test")
    }
}

fn range(start: u64, end: u64) -> ByteRange {
    ByteRange::new(Offset::new(start), Offset::new(end)).expect("合法区间")
}

fn starts(outcome: &geode_search::SearchOutcome) -> Vec<(u64, u64)> {
    outcome
        .hits
        .iter()
        .map(|range| (range.start().get(), range.end().get()))
        .collect()
}

#[test]
fn holes_are_not_searched_and_do_not_break_readable_regions() {
    // 0..4 "AAAB"，4..10 空洞 "SECRET"，10..14 "BBBC"。
    let provider = HolesProvider::new(b"AAABSECRETBBBC".to_vec(), vec![range(0, 4), range(10, 14)]);

    let query = SearchQuery::Hex(HexPattern::from_bytes(b"SECRET"));
    let outcome = search(&provider, &query, &SearchOptions::default(), &|| false).expect("搜索");
    assert!(outcome.hits.is_empty(), "空洞内容不得被命中");

    let query = SearchQuery::Hex(HexPattern::from_bytes(b"BBB"));
    let outcome = search(&provider, &query, &SearchOptions::default(), &|| false).expect("搜索");
    assert_eq!(starts(&outcome), vec![(10, 13)]);

    // 跨空洞的伪匹配：pos 3 的 B + 空洞中的 S。
    let query = SearchQuery::Hex(HexPattern::from_bytes(b"BS"));
    let outcome = search(&provider, &query, &SearchOptions::default(), &|| false).expect("搜索");
    assert!(outcome.hits.is_empty());
}
