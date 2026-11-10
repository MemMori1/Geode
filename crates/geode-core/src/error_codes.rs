//! 诊断码常量表——**由 `cargo xtask docs-errors` 从 `docs/errors.toml` 生成，不要手改**。
//!
//! DESIGN: 00 §12（错误码分配登记表）、18 §10.2 P1
//!
//! 用法：`Diag::error(error_codes::E1002, "…")`。码本身不上屏，句子由 UI 查文案表。

use crate::diag::Code;

/// `E0001`：主题加载时颜色 token 缺失
pub const E0001: Code = Code("E0001");

/// `E0101`：GUI 污染：非 `geode-ui`/`app` 的 crate 引入了 GUI 依赖
pub const E0101: Code = Code("E0101");

/// `E0102`：依赖方向倒置：依赖的层号不小于自身层号
pub const E0102: Code = Code("E0102");

/// `E0103`：依赖边不在允许矩阵里
pub const E0103: Code = Code("E0103");

/// `E0104`：C/C++ 后端依赖没走 feature flag
pub const E0104: Code = Code("E0104");

/// `E0105`：传递依赖把 GUI 拽进了非 UI crate
pub const E0105: Code = Code("E0105");

/// `E0120`：主题 token 不完整或不可用
pub const E0120: Code = Code("E0120");

/// `E0130`：hex 字体不是等宽字体
pub const E0130: Code = Code("E0130");

/// `E1001`：字段值不在枚举声明里
pub const E1001: Code = Code("E1001");

/// `E1002`：字节序列不是该编码的合法文本
pub const E1002: Code = Code("E1002");

/// `E1004`：偏移处 magic 与期望不符
pub const E1004: Code = Code("E1004");

/// `E1005`：断言失败：`#[assert]` 或 `assert()` 求值为假
pub const E1005: Code = Code("E1005");

/// `E1006`：断言失败（02 §8.2 口径，不归 E5 段）
pub const E1006: Code = Code("E1006");

/// `E2001`：执行步数预算耗尽
pub const E2001: Code = Code("E2001");

/// `E2002`：墙钟时间预算耗尽
pub const E2002: Code = Code("E2002");

/// `E2003`：递归或嵌套深度超限
pub const E2003: Code = Code("E2003");

/// `E2004`：解析产物超过大小上限
pub const E2004: Code = Code("E2004");

/// `E2005`：解析被用户取消
pub const E2005: Code = Code("E2005");

/// `E3001`：读取越过文件末尾
pub const E3001: Code = Code("E3001");

/// `E3002`：`@` 恢复点指向文件外
pub const E3002: Code = Code("E3002");

/// `E3003`：单次分配或读取超过上限
pub const E3003: Code = Code("E3003");

/// `E3004`：表达式除零
pub const E3004: Code = Code("E3004");

/// `E3010`：尺寸探测超过探测上限
pub const E3010: Code = Code("E3010");

/// `E4001`：语法错误：此处不该出现该 token
pub const E4001: Code = Code("E4001");

/// `E4002`：类型不匹配
pub const E4002: Code = Code("E4002");

/// `E4003`：使用了未定义的符号
pub const E4003: Code = Code("E4003");

/// `E4004`：定长结构声明大小与成员实际大小不符
pub const E4004: Code = Code("E4004");

/// `E4007`：结构定义相互引用成环，编译期拒绝
pub const E4007: Code = Code("E4007");

/// `E5001`：模板用了 `rva`/`va` 指针但没给 `image_base`
pub const E5001: Code = Code("E5001");

/// `E5002`：缺少解析所需的前置上下文
pub const E5002: Code = Code("E5002");

/// `E6001`：打开数据源时权限不足
pub const E6001: Code = Code("E6001");

/// `E6002`：数据源目标已消失
pub const E6002: Code = Code("E6002");

/// `E6003`：连接断开，正在重连
pub const E6003: Code = Code("E6003");

/// `E6004`：协议超时或响应畸形
pub const E6004: Code = Code("E6004");

/// `E6005`：HEX/SREC 记录格式校验失败
pub const E6005: Code = Code("E6005");

/// `E6006`：环形缓冲已丢弃该区间
pub const E6006: Code = Code("E6006");

/// `E7102`：hex 模式没有任何固定字节
pub const E7102: Code = Code("E7102");

/// `E7103`：正则用了反向引用或环视
pub const E7103: Code = Code("E7103");

/// `E7104`：跨块长匹配可能被漏掉
pub const E7104: Code = Code("E7104");

/// `E7105`：数值搜索的宽度非法或输入不足
pub const E7105: Code = Code("E7105");

/// `E8001`：可视化参数文法错误或含未知 key
pub const E8001: Code = Code("E8001");

/// `E8002`：未知可视化器 id 或格式不支持
pub const E8002: Code = Code("E8002");

/// `E8003`：解码途中失败（截断或损坏）
pub const E8003: Code = Code("E8003");

/// `E8004`：声明尺寸或参数超限，拒绝分配
pub const E8004: Code = Code("E8004");

/// `E8005`：解码超过 2 秒，当前显示缩略图
pub const E8005: Code = Code("E8005");

/// `E8006`：图元超预算且无法降采样
pub const E8006: Code = Code("E8006");

/// `E8007`：插件可视化器 panic，本会话禁用
pub const E8007: Code = Code("E8007");

/// `E9001`：插件目录里有非法文件或 `dlopen` 失败
pub const E9001: Code = Code("E9001");

/// `E9002`：manifest 与插件自报元信息不符
pub const E9002: Code = Code("E9002");

/// `E9003`：插件要求更高的宿主 ABI major
pub const E9003: Code = Code("E9003");

/// `E9004`：插件要求更高的宿主版本
pub const E9004: Code = Code("E9004");

/// `E9005`：插件签名缺失或无效
pub const E9005: Code = Code("E9005");

/// `E9006`：插件能力 id 冲突
pub const E9006: Code = Code("E9006");

/// `E9007`：插件 panic，本会话已禁用
pub const E9007: Code = Code("E9007");

/// `E9008`：插件无响应（死循环）
pub const E9008: Code = Code("E9008");

/// `E9009`：插件进程崩溃（段错误）
pub const E9009: Code = Code("E9009");

/// `E9010`：插件超配额分配
pub const E9010: Code = Code("E9010");

/// `E9011`：插件未实现必需函数
pub const E9011: Code = Code("E9011");

/// `E9012`：热重载失败，已回滚旧版
pub const E9012: Code = Code("E9012");

/// `EA001`：包摘要与索引登记不一致
pub const EA001: Code = Code("EA001");

/// `EA002`：包签名校验失败
pub const EA002: Code = Code("EA002");

/// `EA003`：签名者密钥不在受信列表
pub const EA003: Code = Code("EA003");

/// `EA004`：包内 entry 路径穿越（zip slip）
pub const EA004: Code = Code("EA004");

/// `EA005`：解压炸弹（配额或压缩比超限）
pub const EA005: Code = Code("EA005");

/// `EA006`：磁盘空间不足
pub const EA006: Code = Code("EA006");

/// `EA007`：内容索引解析失败
pub const EA007: Code = Code("EA007");

/// `EA010`：没有可回滚的版本
pub const EA010: Code = Code("EA010");

/// `EA012`：用户版本 `content_api` 不兼容，已回退内置
pub const EA012: Code = Code("EA012");

/// `EA020`：依赖版本冲突
pub const EA020: Code = Code("EA020");

/// `EA021`：依赖成环
pub const EA021: Code = Code("EA021");

/// `EA101`（释义待补，见 18 §10.4 的 27 号文档）
pub const EA101: Code = Code("EA101");

/// `EA102`：manifest 出现可执行钩子字段
pub const EA102: Code = Code("EA102");

/// `EA103`：模板嵌套、递归或源码超限
pub const EA103: Code = Code("EA103");

/// `EA105`：magic 库规模超限
pub const EA105: Code = Code("EA105");

/// `EA106`（释义待补，见 18 §10.4 的 27 号文档）
pub const EA106: Code = Code("EA106");

/// `EA301`：内容含 GPL/AGPL/LGPL 许可，索引硬拒
pub const EA301: Code = Code("EA301");

/// `EB001`：文件版本过新（`TooNew`），拒绝加载且不写盘
pub const EB001: Code = Code("EB001");

/// `EB002`：magic、容器不符或 zip 结构违规
pub const EB002: Code = Code("EB002");

/// `EB003`：文本格式语法错（TOML/UTF-8/BOM/控制字符）
pub const EB003: Code = Code("EB003");

/// `EB004`：必填字段缺失，或 `files[]` 与 entry 集合不等
pub const EB004: Code = Code("EB004");

/// `EB005`：取值或范围违规，或 `params` 里有明文密钥
pub const EB005: Code = Code("EB005");

/// `EB006`：未知枚举值或未知记录 `kind`，降级后继续
pub const EB006: Code = Code("EB006");

/// `EB007`：未知字段（config 保留，项目忽略并告警）
pub const EB007: Code = Code("EB007");

/// `EB008`：悬空引用
pub const EB008: Code = Code("EB008");

/// `EB009`：路径非法（绝对、`..` 逃根、超长、含 `\0`）
pub const EB009: Code = Code("EB009");

/// `EB010`：重复主键
pub const EB010: Code = Code("EB010");

/// `EB011`：记录 CRC 不符
pub const EB011: Code = Code("EB011");

/// `EB012`：记录 `seq` 乱序、重复或跳号
pub const EB012: Code = Code("EB012");

/// `EB013`：文件截断（帧跨文件尾或头长不足）
pub const EB013: Code = Code("EB013");

/// `EB014`：长度不符（`record_count`/`FinalLen`/`Overwrite`）
pub const EB014: Code = Code("EB014");

/// `EB015`：`.geopkg` 摘要不符
pub const EB015: Code = Code("EB015");

/// `EB016`：`.geopkg` 验签失败
pub const EB016: Code = Code("EB016");

/// `EB017`：签名密钥未知或不在受信集
pub const EB017: Code = Code("EB017");

/// `EB018`：签名过期或密钥已吊销
pub const EB018: Code = Code("EB018");

/// `EB019`：`.geopkg` 内 zip slip
pub const EB019: Code = Code("EB019");

/// `EB020`：格式配额超限
pub const EB020: Code = Code("EB020");

/// `EB021`：journal 的 base 指纹不符
pub const EB021: Code = Code("EB021");

/// `EB022`：锁被占用或锁文件不可解析
pub const EB022: Code = Code("EB022");

/// `EB023`：迁移链缺环
pub const EB023: Code = Code("EB023");

/// `EB024`：迁移时字段类型不符
pub const EB024: Code = Code("EB024");

/// `EB025`：迁移后置条件失败，降级丢掉 `layout`/`views`
pub const EB025: Code = Code("EB025");

/// `EB026`：迁移非幂等（实现 bug）
pub const EB026: Code = Code("EB026");

/// `EB027`：超出迁移器保留窗口（>3 个大版本）
pub const EB027: Code = Code("EB027");

/// `EB028`：回滚失败（`.bak` 缺失或损坏）
pub const EB028: Code = Code("EB028");

/// `EB029`：使用了已弃用字段（写侧仍写入）
pub const EB029: Code = Code("EB029");

/// `EB030`：校验收紧导致旧文件不达标
pub const EB030: Code = Code("EB030");

/// `EB101`：迁移源语法无法解析
pub const EB101: Code = Code("EB101");

/// `EB102`：迁移源超限（1MB / 40k 行）
pub const EB102: Code = Code("EB102");

/// `EB201`：构造不可映射（内嵌运行时或副作用宏）
pub const EB201: Code = Code("EB201");

/// `EB202`：类型依赖成环
pub const EB202: Code = Code("EB202");

/// `EB203`：游标顺序不可重建（引用未来字段）
pub const EB203: Code = Code("EB203");

/// `EB301`：产物未过 L1 编译
pub const EB301: Code = Code("EB301");

/// `EB302`：产物未过 L2 执行
pub const EB302: Code = Code("EB302");

/// `EB303`：不变量未过（I4 覆盖或 I5 长度自洽）
pub const EB303: Code = Code("EB303");

/// `EB401`：`origin` 缺失或不完整
pub const EB401: Code = Code("EB401");

/// `EB402`：命中 GPL/AGPL/LGPL 且未声明权利
pub const EB402: Code = Code("EB402");

/// `EB403`：`local-only` 产物尝试 `--pack`
pub const EB403: Code = Code("EB403");

/// `EB404`：与已知 GPL 语料 5-gram 相似度 >0.6
pub const EB404: Code = Code("EB404");

/// `EB501`：CLI IO 失败，或目标已存在且未给 `--force`
pub const EB501: Code = Code("EB501");

/// `EC001`：安全策略拒绝，需要用户显式放行
pub const EC001: Code = Code("EC001");

/// `EC002`：路径越出白名单根，或条目被判不安全
pub const EC002: Code = Code("EC002");

/// `EC003`：输入声明的尺寸或数量超上限（拒绝，不是失败）
pub const EC003: Code = Code("EC003");

/// `EC004`：破坏性操作缺少二次确认（程序性错误）
pub const EC004: Code = Code("EC004");

/// `EC005`：写出物中发现凭据类字段
pub const EC005: Code = Code("EC005");

/// `EC006`：远程目标未经用户同意
pub const EC006: Code = Code("EC006");

/// `EC007`：预算或配额来自输入而非用户设置（程序性错误）
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
