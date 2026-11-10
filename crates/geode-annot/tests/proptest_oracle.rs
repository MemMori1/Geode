//! proptest 对拍：随机区间集合 + 随机查询，与朴素 O(n) 全扫 oracle 比较。
//!
//! DESIGN: 00 §5.3 / 05 §4.7
//!
//! oracle 刻意写得最直白：线性过滤 + 全排序。索引任何剪枝错误（漏块、边界、
//! 排序键、删除残留）都会在这里暴露。

use geode_annot::{Annotation, AnnotationId, AnnotationIndex, AnnotationKind, Layer};
use geode_core::{ByteRange, Offset};
use proptest::prelude::*;

/// 一条随机标注的生成参数。
#[derive(Clone, Copy, Debug)]
struct Spec {
    start: u64,
    len: u64,
    layer: Layer,
    priority: u8,
}

/// oracle 侧的标注副本；`id` 与索引分配一致（从 1 开始、插入序）。
#[derive(Clone, Copy, Debug)]
struct ModelAnn {
    id: u64,
    start: u64,
    end: u64,
    z: u8,
    priority: u8,
    alive: bool,
}

fn spec_strategy() -> impl Strategy<Value = Spec> {
    (0u64..20_000, 0u64..40, 0u8..10, any::<u8>()).prop_map(|(start, len, z, priority)| Spec {
        start,
        len,
        layer: Layer::from_z(z).expect("z 在 0..10"),
        priority,
    })
}

fn build(specs: &[Spec]) -> (AnnotationIndex, Vec<ModelAnn>) {
    let mut index = AnnotationIndex::with_capacity(specs.len());
    let mut model = Vec::with_capacity(specs.len());
    for spec in specs {
        let range =
            ByteRange::from_len(Offset::new(spec.start), spec.len).expect("start + len 不溢出");
        let id = index.insert(Annotation::new(
            range,
            spec.layer,
            spec.priority,
            AnnotationKind::Marker,
        ));
        model.push(ModelAnn {
            id: id.get(),
            start: spec.start,
            end: spec.start + spec.len,
            z: spec.layer.z(),
            priority: spec.priority,
            alive: true,
        });
    }
    (index, model)
}

fn id_vec(ids: impl IntoIterator<Item = AnnotationId>) -> Vec<u64> {
    ids.into_iter().map(AnnotationId::get).collect()
}

fn naive_point(model: &[ModelAnn], at: u64) -> Vec<u64> {
    let mut hits: Vec<&ModelAnn> = model
        .iter()
        .filter(|ann| ann.alive && ann.start <= at && at < ann.end)
        .collect();
    hits.sort_unstable_by_key(|ann| (ann.z, ann.priority, ann.id));
    hits.into_iter().map(|ann| ann.id).collect()
}

fn naive_range(model: &[ModelAnn], start: u64, end: u64) -> Vec<u64> {
    if start >= end {
        return Vec::new();
    }
    let mut hits: Vec<&ModelAnn> = model
        .iter()
        .filter(|ann| ann.alive && ann.start < ann.end && ann.start < end && start < ann.end)
        .collect();
    hits.sort_unstable_by_key(|ann| (ann.z, ann.priority, ann.id));
    hits.into_iter().map(|ann| ann.id).collect()
}

fn naive_deepest(model: &[ModelAnn], at: u64) -> Option<u64> {
    model
        .iter()
        .filter(|ann| ann.alive && ann.start <= at && at < ann.end)
        .max_by_key(|ann| (ann.z, ann.priority, ann.id))
        .map(|ann| ann.id)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// 纯插入后的点查询、区间查询、deepest 与全扫 oracle 一致。
    #[test]
    fn random_queries_match_naive_oracle(
        specs in prop::collection::vec(spec_strategy(), 0..180),
        points in prop::collection::vec(0u64..20_100, 1..64),
        ranges in prop::collection::vec((0u64..20_000, 0u64..80), 1..64),
    ) {
        let (index, model) = build(&specs);
        prop_assert_eq!(index.len(), model.len());

        for at in points {
            prop_assert_eq!(id_vec(index.query_point(Offset::new(at))), naive_point(&model, at));
            prop_assert_eq!(index.deepest_at(Offset::new(at)).map(AnnotationId::get), naive_deepest(&model, at));
        }
        for (start, len) in ranges {
            let end = start + len;
            let range = ByteRange::from_len(Offset::new(start), len).unwrap();
            prop_assert_eq!(id_vec(index.query(range)), naive_range(&model, start, end));
            // 单点退化也走同一条区间查询路径。
            if len == 0 {
                prop_assert!(index.query(range).is_empty());
            }
        }
    }

    /// 随机删除（含重复删除）后，剩余集合仍与 oracle 一致，id 不复用。
    #[test]
    fn random_deletes_match_naive_oracle(
        specs in prop::collection::vec(spec_strategy(), 1..180),
        deletes in prop::collection::vec(0usize..180, 0..48),
        points in prop::collection::vec(0u64..20_100, 1..48),
    ) {
        let (mut index, mut model) = build(&specs);
        for slot in deletes {
            let picked = slot % model.len();
            let ann = &mut model[picked];
            if ann.alive {
                prop_assert!(index.remove(AnnotationId(ann.id)).is_some());
                ann.alive = false;
            } else {
                prop_assert!(index.remove(AnnotationId(ann.id)).is_none());
            }
        }
        let alive = model.iter().filter(|ann| ann.alive).count();
        prop_assert_eq!(index.len(), alive);

        for at in points {
            prop_assert_eq!(id_vec(index.query_point(Offset::new(at))), naive_point(&model, at));
            prop_assert_eq!(index.deepest_at(Offset::new(at)).map(AnnotationId::get), naive_deepest(&model, at));
        }
        for ann in &model {
            prop_assert_eq!(index.get(AnnotationId(ann.id)).is_some(), ann.alive);
        }
    }

    /// 大量相同 start / 相同区间，专门压测块边界与同键排序。
    #[test]
    fn duplicate_ranges_preserve_stable_order(
        starts in prop::collection::vec(0u64..64, 1..256),
        layers in prop::collection::vec(0u8..10, 1..256),
        priorities in prop::collection::vec(any::<u8>(), 1..256),
        at in 0u64..128,
    ) {
        let n = starts.len().min(layers.len()).min(priorities.len());
        let mut index = AnnotationIndex::new();
        let mut expected_keys = Vec::new();
        for i in 0..n {
            let start = starts[i];
            let range = ByteRange::from_len(Offset::new(start), 8).unwrap();
            let layer = Layer::from_z(layers[i]).unwrap();
            let id = index.insert(Annotation::new(
                range,
                layer,
                priorities[i],
                AnnotationKind::Marker,
            ));
            expected_keys.push((layer.z(), priorities[i], id.get()));
        }
        let mut expected: Vec<u64> = expected_keys
            .into_iter()
            .filter(|(_, _, id)| {
                let start = starts[(id - 1) as usize];
                start <= at && at < start + 8
            })
            .map(|(_, _, id)| id)
            .collect();
        expected.sort_unstable();
        let mut expected: Vec<(u8, u8, u64)> = expected
            .into_iter()
            .map(|id| {
                let i = (id - 1) as usize;
                (layers[i], priorities[i], id)
            })
            .collect();
        expected.sort_unstable();
        prop_assert_eq!(
            index.query_point(Offset::new(at)),
            expected
                .into_iter()
                .map(|(_, _, id)| AnnotationId(id))
                .collect::<Vec<_>>()
        );
    }
}
