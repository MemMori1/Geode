//! 基准注册表自检：名字唯一、数量对得上、阻塞项必须写清原因。
//!
//! DESIGN: 21 §5.2、18 §10.2 P4
//!
//! 这条测试替代了"人肉记住哪个 bench 守哪一行"：注册表一旦漏项或改名，
//! CI 立刻红，而不是等到某天发现 `00 §7` 的某行两年没人守。

use geode_bench::{M1_BENCHES, Status};

#[test]
fn names_are_unique_and_complete() {
    assert_eq!(M1_BENCHES.len(), 6, "M1 阶段先守 6 项");
    let mut names: Vec<&str> = M1_BENCHES.iter().map(|b| b.name).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), M1_BENCHES.len(), "bench 名必须唯一");
}

#[test]
fn every_blocked_bench_says_why() {
    for b in M1_BENCHES {
        if let Status::Blocked(why) = b.status {
            assert!(!why.trim().is_empty(), "{} 标了阻塞却没写原因", b.name);
        }
        assert!(
            !b.budget_row.trim().is_empty(),
            "{} 没挂 00 §7 的预算行",
            b.name
        );
    }
}
