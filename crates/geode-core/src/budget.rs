//! 执行预算：给 BinSpec VM 与 AI 工具调用用的硬上限。
//!
//! DESIGN: 02 §9.4、03 §4.3

/// 一次解释执行的资源上限。任一超限即中止并产出 `E2xxx` 诊断。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Budget {
    /// 字节码步数上限（默认 5000 万）。
    pub steps: u64,
    /// 墙钟时间上限（毫秒，默认 5000）。
    pub wall_time_ms: u64,
    /// 变量与字符串分配的上限（字节，默认 1 MiB 个条目）。
    pub mem_vars: u64,
    /// 递归/嵌套深度上限（默认 256）。
    pub depth: u32,
    /// 产出（结构节点 + 字符串）总量上限（字节，默认 64 MiB）。
    pub output_bytes: u64,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            steps: 50_000_000,
            wall_time_ms: 5_000,
            mem_vars: 1_000_000,
            depth: 256,
            output_bytes: 64 << 20,
        }
    }
}

impl Budget {
    /// AI 生成路径下更保守的预算（03 §4.3：单次工具调用）。
    pub fn ai_tool() -> Self {
        Self {
            steps: 10_000_000,
            wall_time_ms: 2_000,
            ..Self::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ai_budget_is_stricter() {
        let d = Budget::default();
        let a = Budget::ai_tool();
        assert!(a.steps < d.steps);
        assert!(a.wall_time_ms < d.wall_time_ms);
    }
}
