//! 诊断码常量表——**由 `cargo xtask docs-errors` 从 `docs/errors.toml` 生成，不要手改**。
//!
//! DESIGN: 00 §12（错误码分配登记表）、18 §10.2 P1
//!
//! 用法：`Diag::error(error_codes::E1002, "…")`。码本身不上屏，句子由 UI 查文案表。

use crate::diag::Code;

/// `E0001`（释义待补，见 18 §10.4 的 27 号文档）
pub const E0001: Code = Code("E0001");

/// `E0101`（释义待补，见 18 §10.4 的 27 号文档）
pub const E0101: Code = Code("E0101");

/// `E0102`（释义待补，见 18 §10.4 的 27 号文档）
pub const E0102: Code = Code("E0102");

/// `E0103`（释义待补，见 18 §10.4 的 27 号文档）
pub const E0103: Code = Code("E0103");

/// `E0104`（释义待补，见 18 §10.4 的 27 号文档）
pub const E0104: Code = Code("E0104");

/// `E0105`（释义待补，见 18 §10.4 的 27 号文档）
pub const E0105: Code = Code("E0105");

/// `E0120`：主题 `nord` 缺少 3 个颜色定义：`layer.ai`、`hex.null-byte`、`severity.info`。
pub const E0120: Code = Code("E0120");

/// `E0130`：字体 `MyMono.ttf` 不是等宽字体：`i` 与 `W` 宽度相差 31%。
pub const E0130: Code = Code("E0130");

/// `E1001`：enum value not in declaration
pub const E1001: Code = Code("E1001");

/// `E1002`：invalid byte sequence for encoding
pub const E1002: Code = Code("E1002");

/// `E1004`：偏移 0x1000 的 magic 不匹配：期望 `50 4B 03 04`，实际 `00 00 00 00`。
pub const E1004: Code = Code("E1004");

/// `E1005`：assertion failed
pub const E1005: Code = Code("E1005");

/// `E1006`：数据不匹配·断言：`#[assert]` 或 `assert()` 失败（**不归 E5**，E5 只表示前置上下文缺失）
pub const E1006: Code = Code("E1006");

/// `E2001`：模板在第 3 个循环第 412 次迭代处耗尽执行预算（1 秒）。
pub const E2001: Code = Code("E2001");

/// `E2002`：wall time budget exceeded
pub const E2002: Code = Code("E2002");

/// `E2003`：recursion/depth limit reached
pub const E2003: Code = Code("E2003");

/// `E2004`：output size limit exceeded
pub const E2004: Code = Code("E2004");

/// `E2005`：cancelled by user
pub const E2005: Code = Code("E2005");

/// `E3001`：read out of bounds
pub const E3001: Code = Code("E3001");

/// `E3002`：resume target outside file
pub const E3002: Code = Code("E3002");

/// `E3003`：allocation exceeds limit
pub const E3003: Code = Code("E3003");

/// `E3004`：division by zero
pub const E3004: Code = Code("E3004");

/// `E3010`（释义待补，见 18 §10.4 的 27 号文档）
pub const E3010: Code = Code("E3010");

/// `E4001`：unexpected token
pub const E4001: Code = Code("E4001");

/// `E4002`：type mismatch
pub const E4002: Code = Code("E4002");

/// `E4003`：undefined symbol
pub const E4003: Code = Code("E4003");

/// `E4004`：fixed struct size mismatch
pub const E4004: Code = Code("E4004");

/// `E4007`（释义待补，见 18 §10.4 的 27 号文档）
pub const E4007: Code = Code("E4007");

/// `E5001`：image_base not provided
pub const E5001: Code = Code("E5001");

/// `E5002`：required context missing
pub const E5002: Code = Code("E5002");

/// `E6001`：无法读取 `/dev/rdisk2`：需要管理员权限。
pub const E6001: Code = Code("E6001");

/// `E6002`：目标进程 4812 已退出，内存不再可读。
pub const E6002: Code = Code("E6002");

/// `E6003`（释义待补，见 18 §10.4 的 27 号文档）
pub const E6003: Code = Code("E6003");

/// `E6004`（释义待补，见 18 §10.4 的 27 号文档）
pub const E6004: Code = Code("E6004");

/// `E6005`（释义待补，见 18 §10.4 的 27 号文档）
pub const E6005: Code = Code("E6005");

/// `E6006`（释义待补，见 18 §10.4 的 27 号文档）
pub const E6006: Code = Code("E6006");

/// `E7102`：无法搜索 `?? ?? ??`：模式里没有固定字节。
pub const E7102: Code = Code("E7102");

/// `E7103`：正则不支持反向引用 `\1` 与环视 `(?=)`。
pub const E7103: Code = Code("E7103");

/// `E7104`（释义待补，见 18 §10.4 的 27 号文档）
pub const E7104: Code = Code("E7104");

/// `E7105`（释义待补，见 18 §10.4 的 27 号文档）
pub const E7105: Code = Code("E7105");

/// `E8001`（释义待补，见 18 §10.4 的 27 号文档）
pub const E8001: Code = Code("E8001");

/// `E8002`（释义待补，见 18 §10.4 的 27 号文档）
pub const E8002: Code = Code("E8002");

/// `E8003`（释义待补，见 18 §10.4 的 27 号文档）
pub const E8003: Code = Code("E8003");

/// `E8004`：无法显示图片：头部声明 60000×60000 像素。
pub const E8004: Code = Code("E8004");

/// `E8005`：可视化器 2 秒内未完成解码，当前显示缩略图。
pub const E8005: Code = Code("E8005");

/// `E8006`（释义待补，见 18 §10.4 的 27 号文档）
pub const E8006: Code = Code("E8006");

/// `E8007`（释义待补，见 18 §10.4 的 27 号文档）
pub const E8007: Code = Code("E8007");

/// `E9001`（释义待补，见 18 §10.4 的 27 号文档）
pub const E9001: Code = Code("E9001");

/// `E9002`（释义待补，见 18 §10.4 的 27 号文档）
pub const E9002: Code = Code("E9002");

/// `E9003`：插件 `hexdecoder` 需要宿主 ABI 2.x，当前为 1.4。
pub const E9003: Code = Code("E9003");

/// `E9004`（释义待补，见 18 §10.4 的 27 号文档）
pub const E9004: Code = Code("E9004");

/// `E9005`（释义待补，见 18 §10.4 的 27 号文档）
pub const E9005: Code = Code("E9005");

/// `E9006`（释义待补，见 18 §10.4 的 27 号文档）
pub const E9006: Code = Code("E9006");

/// `E9007`：插件 `quickstat` 解析时崩溃，本会话已禁用。
pub const E9007: Code = Code("E9007");

/// `E9008`（释义待补，见 18 §10.4 的 27 号文档）
pub const E9008: Code = Code("E9008");

/// `E9009`（释义待补，见 18 §10.4 的 27 号文档）
pub const E9009: Code = Code("E9009");

/// `E9010`（释义待补，见 18 §10.4 的 27 号文档）
pub const E9010: Code = Code("E9010");

/// `E9011`（释义待补，见 18 §10.4 的 27 号文档）
pub const E9011: Code = Code("E9011");

/// `E9012`（释义待补，见 18 §10.4 的 27 号文档）
pub const E9012: Code = Code("E9012");

/// `EA001`：无法安装 `tpl.zip 1.4.0`：文件摘要与索引登记不一致。
pub const EA001: Code = Code("EA001");

/// `EA002`：无法安装 `theme.nord 1.2.0`：签名校验失败。
pub const EA002: Code = Code("EA002");

/// `EA003`：tpl.xyz 0.3.0` 的签名者 `ed25519:deadbeef` 不在受信列表。
pub const EA003: Code = Code("EA003");

/// `EA004`（释义待补，见 18 §10.4 的 27 号文档）
pub const EA004: Code = Code("EA004");

/// `EA005`（释义待补，见 18 §10.4 的 27 号文档）
pub const EA005: Code = Code("EA005");

/// `EA006`（释义待补，见 18 §10.4 的 27 号文档）
pub const EA006: Code = Code("EA006");

/// `EA007`（释义待补，见 18 §10.4 的 27 号文档）
pub const EA007: Code = Code("EA007");

/// `EA010`（释义待补，见 18 §10.4 的 27 号文档）
pub const EA010: Code = Code("EA010");

/// `EA012`（释义待补，见 18 §10.4 的 27 号文档）
pub const EA012: Code = Code("EA012");

/// `EA020`（释义待补，见 18 §10.4 的 27 号文档）
pub const EA020: Code = Code("EA020");

/// `EA021`（释义待补，见 18 §10.4 的 27 号文档）
pub const EA021: Code = Code("EA021");

/// `EA101`（释义待补，见 18 §10.4 的 27 号文档）
pub const EA101: Code = Code("EA101");

/// `EA102`（释义待补，见 18 §10.4 的 27 号文档）
pub const EA102: Code = Code("EA102");

/// `EA103`（释义待补，见 18 §10.4 的 27 号文档）
pub const EA103: Code = Code("EA103");

/// `EA105`（释义待补，见 18 §10.4 的 27 号文档）
pub const EA105: Code = Code("EA105");

/// `EA106`（释义待补，见 18 §10.4 的 27 号文档）
pub const EA106: Code = Code("EA106");

/// `EA301`（释义待补，见 18 §10.4 的 27 号文档）
pub const EA301: Code = Code("EA301");

/// `EB001`（释义待补，见 18 §10.4 的 27 号文档）
pub const EB001: Code = Code("EB001");

/// `EB002`（释义待补，见 18 §10.4 的 27 号文档）
pub const EB002: Code = Code("EB002");

/// `EB003`（释义待补，见 18 §10.4 的 27 号文档）
pub const EB003: Code = Code("EB003");

/// `EB004`（释义待补，见 18 §10.4 的 27 号文档）
pub const EB004: Code = Code("EB004");

/// `EB005`（释义待补，见 18 §10.4 的 27 号文档）
pub const EB005: Code = Code("EB005");

/// `EB006`（释义待补，见 18 §10.4 的 27 号文档）
pub const EB006: Code = Code("EB006");

/// `EB007`（释义待补，见 18 §10.4 的 27 号文档）
pub const EB007: Code = Code("EB007");

/// `EB008`（释义待补，见 18 §10.4 的 27 号文档）
pub const EB008: Code = Code("EB008");

/// `EB009`（释义待补，见 18 §10.4 的 27 号文档）
pub const EB009: Code = Code("EB009");

/// `EB010`（释义待补，见 18 §10.4 的 27 号文档）
pub const EB010: Code = Code("EB010");

/// `EB011`（释义待补，见 18 §10.4 的 27 号文档）
pub const EB011: Code = Code("EB011");

/// `EB012`（释义待补，见 18 §10.4 的 27 号文档）
pub const EB012: Code = Code("EB012");

/// `EB013`（释义待补，见 18 §10.4 的 27 号文档）
pub const EB013: Code = Code("EB013");

/// `EB014`（释义待补，见 18 §10.4 的 27 号文档）
pub const EB014: Code = Code("EB014");

/// `EB015`（释义待补，见 18 §10.4 的 27 号文档）
pub const EB015: Code = Code("EB015");

/// `EB016`（释义待补，见 18 §10.4 的 27 号文档）
pub const EB016: Code = Code("EB016");

/// `EB017`（释义待补，见 18 §10.4 的 27 号文档）
pub const EB017: Code = Code("EB017");

/// `EB018`（释义待补，见 18 §10.4 的 27 号文档）
pub const EB018: Code = Code("EB018");

/// `EB019`（释义待补，见 18 §10.4 的 27 号文档）
pub const EB019: Code = Code("EB019");

/// `EB020`（释义待补，见 18 §10.4 的 27 号文档）
pub const EB020: Code = Code("EB020");

/// `EB021`（释义待补，见 18 §10.4 的 27 号文档）
pub const EB021: Code = Code("EB021");

/// `EB022`（释义待补，见 18 §10.4 的 27 号文档）
pub const EB022: Code = Code("EB022");

/// `EB023`（释义待补，见 18 §10.4 的 27 号文档）
pub const EB023: Code = Code("EB023");

/// `EB024`（释义待补，见 18 §10.4 的 27 号文档）
pub const EB024: Code = Code("EB024");

/// `EB025`（释义待补，见 18 §10.4 的 27 号文档）
pub const EB025: Code = Code("EB025");

/// `EB026`（释义待补，见 18 §10.4 的 27 号文档）
pub const EB026: Code = Code("EB026");

/// `EB027`（释义待补，见 18 §10.4 的 27 号文档）
pub const EB027: Code = Code("EB027");

/// `EB028`（释义待补，见 18 §10.4 的 27 号文档）
pub const EB028: Code = Code("EB028");

/// `EB029`：全部
pub const EB029: Code = Code("EB029");

/// `EB030`（释义待补，见 18 §10.4 的 27 号文档）
pub const EB030: Code = Code("EB030");

/// `EB101`：源语法无法解析
pub const EB101: Code = Code("EB101");

/// `EB102`：源超限（1MB / 40k 行）
pub const EB102: Code = Code("EB102");

/// `EB201`：构造不可映射（内嵌运行时/副作用宏）
pub const EB201: Code = Code("EB201");

/// `EB202`：类型依赖成环
pub const EB202: Code = Code("EB202");

/// `EB203`：游标顺序不可重建（引用未来字段）
pub const EB203: Code = Code("EB203");

/// `EB301`：产物未过 L1 编译
pub const EB301: Code = Code("EB301");

/// `EB302`：产物未过 L2 执行
pub const EB302: Code = Code("EB302");

/// `EB303`：不变量未过（I4 覆盖/I5 长度自洽）
pub const EB303: Code = Code("EB303");

/// `EB401`：origin` 缺失或不完整
pub const EB401: Code = Code("EB401");

/// `EB402`：命中 GPL/AGPL/LGPL 且未声明权利
pub const EB402: Code = Code("EB402");

/// `EB403`：local-only` 产物尝试 `--pack
pub const EB403: Code = Code("EB403");

/// `EB404`：与已知 GPL 语料 5-gram 相似度 > 0.6
pub const EB404: Code = Code("EB404");

/// `EB501`：CLI IO / 目标已存在且无 `--force
pub const EB501: Code = Code("EB501");

/// `EC001`：安全策略拒绝（需用户显式放行）
pub const EC001: Code = Code("EC001");

/// `EC002`：路径越出白名单根或被判为不安全条目
pub const EC002: Code = Code("EC002");

/// `EC003`：输入声明的尺寸/数量超上限（**拒绝**，非失败）
pub const EC003: Code = Code("EC003");

/// `EC004`：破坏性操作缺少二次确认（程序性错误，仅内部）
pub const EC004: Code = Code("EC004");

/// `EC005`：将在写出物中发现凭据类字段
pub const EC005: Code = Code("EC005");

/// `EC006`：远程目标未经用户同意（`providers` 段静默连网）
pub const EC006: Code = Code("EC006");

/// `EC007`：预算/配额由**输入**而非用户设置提供（程序性错误）
pub const EC007: Code = Code("EC007");

/// 全部已登记诊断码（按码升序，与 `docs/errors.toml` 一致）。
#[rustfmt::skip]
pub const ALL: &[Code] = &[
    E0001,
    E0101,
    E0102,
    E0103,
    E0104,
    E0105,
    E0120,
    E0130,
    E1001,
    E1002,
    E1004,
    E1005,
    E1006,
    E2001,
    E2002,
    E2003,
    E2004,
    E2005,
    E3001,
    E3002,
    E3003,
    E3004,
    E3010,
    E4001,
    E4002,
    E4003,
    E4004,
    E4007,
    E5001,
    E5002,
    E6001,
    E6002,
    E6003,
    E6004,
    E6005,
    E6006,
    E7102,
    E7103,
    E7104,
    E7105,
    E8001,
    E8002,
    E8003,
    E8004,
    E8005,
    E8006,
    E8007,
    E9001,
    E9002,
    E9003,
    E9004,
    E9005,
    E9006,
    E9007,
    E9008,
    E9009,
    E9010,
    E9011,
    E9012,
    EA001,
    EA002,
    EA003,
    EA004,
    EA005,
    EA006,
    EA007,
    EA010,
    EA012,
    EA020,
    EA021,
    EA101,
    EA102,
    EA103,
    EA105,
    EA106,
    EA301,
    EB001,
    EB002,
    EB003,
    EB004,
    EB005,
    EB006,
    EB007,
    EB008,
    EB009,
    EB010,
    EB011,
    EB012,
    EB013,
    EB014,
    EB015,
    EB016,
    EB017,
    EB018,
    EB019,
    EB020,
    EB021,
    EB022,
    EB023,
    EB024,
    EB025,
    EB026,
    EB027,
    EB028,
    EB029,
    EB030,
    EB101,
    EB102,
    EB201,
    EB202,
    EB203,
    EB301,
    EB302,
    EB303,
    EB401,
    EB402,
    EB403,
    EB404,
    EB501,
    EC001,
    EC002,
    EC003,
    EC004,
    EC005,
    EC006,
    EC007,
];

#[cfg(test)]
mod tests {
    use super::*;

    const SEGMENTS: &[&str] = &[
        "E0", "E1", "E2", "E3", "E4", "E5", "E6", "E7", "E8", "E9", "EA", "EB", "EC",
    ];

    #[test]
    fn all_is_sorted_and_unique() {
        let ids: Vec<&str> = ALL.iter().map(|c| c.0).collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(ids, sorted, "ALL 必须按码升序且无重复");
    }

    #[test]
    fn segments_are_registered() {
        for c in ALL {
            assert!(SEGMENTS.contains(&c.segment()), "{} 的段位未登记", c);
        }
    }
}
