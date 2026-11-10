//! Verifier：在结构树上跑不变量。
//!
//! DESIGN: 03-ai-dsl-generation.md §5（10 条不变量 I1~I10）、§12（Verifier 先于 AI）
//!
//! 这里实现的是**与语言无关、只看 `Node` 树**的那几条；I1（编译通过）由调用方保证，
//! I3（跨采样一致）需要多份样本，I5（长度字段自洽）需要知道哪个字段是长度——
//! 这两条留给 `AiVerifier`（M5）。核心原则：**宁可少报，不可编造**。

use crate::node::Node;
use geode_core::diag::{Code, Diag};
use geode_core::{ByteRange, Offset};

/// 不变量配置。
#[derive(Debug, Clone, Copy)]
pub struct VerifyCfg {
    /// 叶子覆盖率的及格线（I4）。默认 0.4。
    pub min_coverage: f64,
    /// 兄弟节点之间允许的最大空洞（I10）。默认 64 KiB。
    pub max_gap: u64,
    /// 单个父节点下允许的最大子节点数（I7）。默认 1_000_000。
    pub max_children: usize,
    /// 是否要求兄弟节点按偏移单调（I9）。默认 true。
    pub require_monotonic: bool,
}

impl Default for VerifyCfg {
    fn default() -> Self {
        Self {
            min_coverage: 0.4,
            max_gap: 64 * 1024,
            max_children: 1_000_000,
            require_monotonic: true,
        }
    }
}

/// 验证报告。
#[derive(Debug, Clone)]
pub struct VerifyReport {
    /// 诊断列表（按 `Node::walk` 的深度优先顺序）。
    pub diags: Vec<Diag>,
    /// 叶子覆盖的字节 / 数据总长。
    pub coverage: f64,
    /// 节点总数。
    pub nodes: usize,
    /// 是否全部通过（没有 error/fatal 级诊断）。
    pub ok: bool,
}

impl VerifyReport {
    /// 是否存在 error/fatal 级诊断。
    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.diags.iter().any(|d| {
            matches!(
                d.severity,
                geode_core::diag::Severity::Error | geode_core::diag::Severity::Fatal
            )
        })
    }
}

/// 在结构树上跑 I2/I4/I6/I7/I9/I10。
#[must_use]
pub fn verify(root: &Node, data_len: u64, cfg: &VerifyCfg) -> VerifyReport {
    let mut diags = Vec::new();
    let mut nodes = 0usize;

    walk(root, data_len, cfg, &mut diags, &mut nodes);

    let covered = root.covered_bytes();
    let coverage = if data_len == 0 {
        1.0
    } else {
        (covered as f64 / data_len as f64).clamp(0.0, 1.0)
    };
    if data_len > 0 && coverage < cfg.min_coverage {
        diags.push(
            Diag::warning(
                Code("E1006"),
                format!(
                    "字节覆盖率 {:.1}% 低于 {:.0}%（I4）",
                    coverage * 100.0,
                    cfg.min_coverage * 100.0
                ),
            )
            .with_path(root.path.clone()),
        );
    }

    let ok = !diags.iter().any(|d| {
        matches!(
            d.severity,
            geode_core::diag::Severity::Error | geode_core::diag::Severity::Fatal
        )
    });
    VerifyReport {
        diags,
        coverage,
        nodes,
        ok,
    }
}

fn walk(node: &Node, data_len: u64, cfg: &VerifyCfg, out: &mut Vec<Diag>, nodes: &mut usize) {
    *nodes += 1;

    // I2：边界内。空节点（size=0）不算越界，但 offset 不能超出。
    if node.offset > data_len || node.offset.saturating_add(node.size) > data_len {
        let mut diag = Diag::error(
            Code("E3001"),
            format!(
                "节点 [{}, {}) 超出数据长度 {data_len}（I2）",
                node.offset,
                node.offset.saturating_add(node.size)
            ),
        )
        .with_path(node.path.clone());
        if let Some(r) = ByteRange::from_len(Offset::new(node.offset), node.size) {
            diag = diag.with_range(r);
        }
        out.push(diag);
    }

    // I7：数组元素数上限（防"读到 10 亿个元素"的模板）。
    if node.children.len() > cfg.max_children {
        out.push(
            Diag::error(
                Code("E3003"),
                format!(
                    "子节点数 {} 超过上限 {}（I7）",
                    node.children.len(),
                    cfg.max_children
                ),
            )
            .with_path(node.path.clone()),
        );
    }

    // I6 + I9 + I10：子节点必须落在父节点内、按偏移单调、空洞不超限。
    let mut prev_end = node.offset;
    for child in &node.children {
        if child.offset < node.offset
            || child.offset.saturating_add(child.size) > node.offset.saturating_add(node.size)
        {
            out.push(
                Diag::error(
                    Code("E3001"),
                    format!(
                        "子节点 {} 未被父节点 {:#x} 包含（I6）",
                        child.path, node.offset
                    ),
                )
                .with_path(child.path.clone()),
            );
        }
        if cfg.require_monotonic && child.offset < prev_end {
            out.push(
                Diag::warning(
                    Code("E1006"),
                    format!(
                        "子节点 {} 的偏移 {} 逆序或与前一节点重叠（I9/I6）",
                        child.path, child.offset
                    ),
                )
                .with_path(child.path.clone()),
            );
        }
        let gap = child.offset.saturating_sub(prev_end);
        if gap > cfg.max_gap {
            out.push(
                Diag::warning(
                    Code("E1006"),
                    format!("{} 之前有 {gap} 字节空洞（I10）", child.path),
                )
                .with_path(child.path.clone()),
            );
        }
        prev_end = prev_end.max(child.offset.saturating_add(child.size));
        walk(child, data_len, cfg, out, nodes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::Value;

    fn tree() -> Node {
        let mut root = Node::branch("file", "File", 0, 16);
        root.push_child(Node::leaf(
            "magic",
            "u32",
            0,
            4,
            Some(Value::UInt(0x8950_4E47)),
        ));
        root.push_child(Node::leaf("len", "u32", 4, 4, Some(Value::UInt(16))));
        root
    }

    #[test]
    fn happy_path_passes() {
        let r = verify(&tree(), 16, &VerifyCfg::default());
        assert!(r.ok, "{:?}", r.diags);
        assert_eq!(r.nodes, 3);
        assert!((r.coverage - 0.5).abs() < 1e-9);
    }

    #[test]
    fn out_of_bounds_is_error() {
        let mut root = Node::branch("file", "File", 0, 32);
        root.push_child(Node::leaf("x", "u32", 30, 4, None));
        let r = verify(&root, 16, &VerifyCfg::default());
        assert!(!r.ok);
        assert!(r.diags.iter().any(|d| d.code == Code("E3001")));
    }

    #[test]
    fn big_gap_warns() {
        let mut root = Node::branch("file", "File", 0, 200_000);
        root.push_child(Node::leaf("a", "u8", 0, 1, None));
        root.push_child(Node::leaf("b", "u8", 150_000, 1, None));
        let r = verify(&root, 200_000, &VerifyCfg::default());
        assert!(r.diags.iter().any(|d| d.detail.contains("空洞")));
    }

    #[test]
    fn child_outside_parent_is_error() {
        let mut root = Node::branch("file", "File", 0, 8);
        root.push_child(Node::leaf("x", "u32", 6, 4, None));
        let r = verify(&root, 16, &VerifyCfg::default());
        assert!(!r.ok);
    }

    #[test]
    fn low_coverage_warns() {
        let mut root = Node::branch("file", "File", 0, 1000);
        root.push_child(Node::leaf("a", "u8", 0, 1, None));
        let r = verify(&root, 1000, &VerifyCfg::default());
        assert!(r.diags.iter().any(|d| d.detail.contains("覆盖率")));
    }
}
