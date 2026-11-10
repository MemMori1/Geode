//! 基准注册表：**bench 与 `00 §7` 性能预算行的对应关系**。
//!
//! DESIGN: 21-benchmark-suite.md §5.2、18 §10.2 P4
//!
//! 规矩（21 §5.2）：`00 §7` 的每一行**只允许一个 bench 守它**，否则"哪个红了算谁的"
//! 会变成扯皮。这里把清单写成代码，`cargo bench` 与未来的 `xtask bench audit` 共用。

#![deny(missing_docs)]

/// 一项基准的登记条目。
pub struct Bench {
    /// bench 名（与 `[[bench]]` / 函数名一致）。
    pub name: &'static str,
    /// 它守的 `00 §7` 预算行（人类可读的摘录）。
    pub budget_row: &'static str,
    /// 是否已实现；未实现的必须写清依赖哪个任务。
    pub status: Status,
}

/// 实现状态。
pub enum Status {
    /// 已实现，可在 CI 跑。
    Ready,
    /// 未实现，附阻塞原因（任务号）。
    Blocked(&'static str),
}

/// 21 §5.2 里与 M1 相关的 6 项（全量 27 项随里程碑补齐）。
pub const M1_BENCHES: &[Bench] = &[
    Bench {
        name: "open_4gb",
        budget_row: "00 §7：打开 4 GiB <200 ms（M0①）",
        status: Status::Ready,
    },
    Bench {
        name: "read_64k",
        budget_row: "00 §7：随机 64 KiB 读 p99 <1 ms",
        status: Status::Ready,
    },
    Bench {
        name: "scroll_p99",
        budget_row: "00 §7：滚动 p99 <25 ms（M0②）",
        status: Status::Blocked("geode-ui 网格（M0 收尾）"),
    },
    Bench {
        name: "save_1gb",
        budget_row: "00 §7：保存 1 GiB >500 MB/s",
        status: Status::Ready,
    },
    Bench {
        name: "edit_random",
        budget_row: "00 §7：1e5 次随机编辑单步 p99 <1 ms（21 B3）",
        status: Status::Ready,
    },
    Bench {
        name: "undo_all",
        budget_row: "M1 验收②：1 万次随机编辑后撤销全部，字节与原文逐字节相等",
        status: Status::Ready,
    },
];
