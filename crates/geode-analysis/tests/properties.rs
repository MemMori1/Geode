//! 属性测试：直方图计数守恒、分块读哈希与一次性哈希一致。

use geode_analysis::{ByteHistogram, CancelToken, HashAlgo, hash_hex, hash_stream};
use geode_core::{Offset, ProviderId};
use geode_provider::{Caps, MemoryProvider, Provider, ReadReport, SizeInfo};
use proptest::prelude::*;

/// 每次最多返回 `max_read` 字节的包装器，用来强制跨块短读路径。
#[derive(Debug)]
struct ChoppyProvider {
    inner: MemoryProvider,
    max_read: usize,
}

impl Provider for ChoppyProvider {
    fn id(&self) -> ProviderId {
        self.inner.id()
    }

    fn display_name(&self) -> String {
        format!("短读: {}", self.inner.display_name())
    }

    fn caps(&self) -> Caps {
        self.inner.caps()
    }

    fn size(&self) -> SizeInfo {
        self.inner.size()
    }

    fn read_range(
        &self,
        at: Offset,
        buf: &mut [u8],
    ) -> Result<ReadReport, geode_provider::ProviderError> {
        let limit = buf.len().min(self.max_read);
        self.inner.read_range(at, &mut buf[..limit])
    }
}

proptest! {
    #[test]
    fn histogram_total_equals_input_len(data in prop::collection::vec(any::<u8>(), 0..4096)) {
        let mut histogram = ByteHistogram::new();
        histogram.observe_slice(&data);
        prop_assert_eq!(histogram.total(), data.len() as u64);
        let count_sum: u64 = histogram.counts().iter().sum();
        prop_assert_eq!(count_sum, data.len() as u64);
        prop_assert_eq!(histogram.top_n(256).iter().map(|(_, count)| count).sum::<u64>(), data.len() as u64);
    }

    #[test]
    fn chunked_hash_equals_one_shot(
        data in prop::collection::vec(any::<u8>(), 0..5000),
        max_read in 1usize..64,
    ) {
        let provider = ChoppyProvider {
            inner: MemoryProvider::new("choppy", data.clone()),
            max_read,
        };
        for algo in HashAlgo::ALL {
            let outcome = hash_stream(&provider, algo, &CancelToken::new()).unwrap();
            prop_assert_eq!(outcome.bytes, data.len() as u64);
            prop_assert_eq!(outcome.hex, hash_hex(algo, &data));
        }
    }
}
