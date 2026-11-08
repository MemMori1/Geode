import re

base = "/Users/edwa2d4th/Desktop/hex/.tmp/"

def rd(p):
    return open(base + p, encoding="utf-8").read().split("\n")

def wr(p, ls):
    open(base + p, "w", encoding="utf-8").write("\n".join(ls))

def drop(ls, subs, tag):
    hit, out = set(), []
    for l in ls:
        m = [s for s in subs if s in l]
        if m:
            hit.add(m[0])
            continue
        out.append(l)
    miss = [s for s in subs if s not in hit]
    if miss:
        print("MISS-drop", tag, miss)
    return out

def repl(ls, pairs, tag):
    txt = "\n".join(ls)
    for a, b in pairs:
        if a not in txt:
            print("MISS-repl", tag, a.split("\n")[0][:50])
        txt = txt.replace(a, b, 1)
    return txt.split("\n")

# ---------------- part1 ----------------
p1 = rd("part1.md")
p1 = repl(p1, [
 ("| D9 | 错误可聚合计数，但「至少一条可见」不可让渡 | 聚合 ≠ 静默 |\n| D10 | 空态与首启引导是既有命令的镜像，不新增功能面 | 引导不得成为使用前提 |",
  "| D9 | 错误可聚合但至少一条可见；空态与引导只镜像既有命令 | 聚合 ≠ 静默，引导不得成为使用前提 |"),
 ("pub struct UiAction { pub id: ActionId, pub label_key: TextKey,\n                      pub kind: ActionKind, pub destructive: bool }",
  "pub struct UiAction { pub id: ActionId, pub label_key: TextKey, pub kind: ActionKind, pub destructive: bool }"),
 ("pub enum ActionKind { FocusOffset(Offset), Retry, SwitchReadOnly, SaveAs, ReloadFromDisk,\n                      ExportPatch, OpenSettings(&'static str), TrustKey(&'static str), Dismiss }",
  "pub enum ActionKind { FocusOffset(Offset), Retry, SwitchReadOnly, SaveAs, ReloadFromDisk, ExportPatch, OpenSettings(&'static str), TrustKey(&'static str), Dismiss }"),
 ("pub struct CopyEntry { what_key, why_key, fix_key: TextKey,\n                       severity: Severity, default_action: Option<ActionKind> }",
  "pub struct CopyEntry { what_key, why_key, fix_key: TextKey, severity: Severity, default_action: Option<ActionKind> }"),
 ("pub struct Journey { pub id: JourneyId,                // A..F，与 §4.1、§9 脚本同名\n                     pub entry: &'static [CommandId], pub steps: &'static [StepId],\n                     pub success: StepId, pub failures: &'static [FailBranch],\n                     pub empty: Option<StepId>, pub feel: FeelBudget }\npub struct FeelBudget { pub first_content_ms: u32, pub input_to_paint_ms: u32, pub cancel_p99_ms: u32 }",
  "pub struct Journey { pub id: JourneyId,                // A..F，与 §4.1、§9 脚本同名\n                     pub entry, steps, success, empty, failures, feel }        // 六断面，见下\npub struct FeelBudget { first_content_ms, input_to_paint_ms, cancel_p99_ms: u32 }"),
 ("每条给六个断面：触发入口 / 步骤序列 / 成功态 / 失败态 / 空态 / 关键性能感受。步骤里的界面文字即最终文案。\n\n**J-A 打开 20GB 文件并滚到 90%**\n\n",
  "每条给六个断面：触发 / 步骤 / 成功态 / 失败态 / 空态 / 感受；步骤里的界面文字即最终文案。\n**J-A 打开 20GB 文件并滚到 90%**\n"),
 ("**J-B 定位并覆盖编辑 3 个字节、撤销、保存（含冲突与恢复两支）**\n\n", "**J-B 定位并覆盖编辑 3 个字节、撤销、保存（含冲突与恢复两支）**\n"),
 ("**J-C 四种搜索各一次并跳转（含「结果已过期」策略）**\n\n", "**J-C 四种搜索各一次并跳转（含「结果已过期」策略）**\n"),
 ("**J-D 用 BinSpec 解析（magic → AI 建议 → 置信三色 → 局部降级）**\n\n", "**J-D 用 BinSpec 解析（magic → AI 建议 → 置信三色 → 局部降级）**\n"),
 ("**J-E 两个文件 diff 并导出报告**\n\n", "**J-E 两个文件 diff 并导出报告**\n"),
 ("**J-F 安装一个内容包（成功路径 → 签名失败 → 回滚）**\n\n", "**J-F 安装一个内容包（成功路径 → 签名失败 → 回滚）**\n"),
], "p1")
p1 = drop(p1, ["| D2 | 六条旅程 = 脚本编号 J-A…J-F", "| 22 | 25 条样例之外的错误码→人话映射"], "p1")
# renumber decision rows D1..D8
n = 0
for i, l in enumerate(p1):
    if re.match(r"^\| D\d+ \|", l):
        n += 1
        p1[i] = re.sub(r"^\| D\d+ \|", "| D%d |" % n, l)
wr("part1.md", p1)
print("part1", len(p1))

# ---------------- wire3 ----------------
w = [l for l in rd("wire3.md") if l.strip() != ""]
w = drop(w, ["|   report.bin     已修改", "| 00002C80", "| 00001A38  74 78 74 00 00   | + |", "|       blocks[]       0.71"], "wire")
wr("wire3.md", w)
print("wire", len(w))

# ---------------- part2b ----------------
p2 = rd("part2b.md")
p2 = drop(p2, [
 "| 保存进度 | 更新 ≥4Hz",
 "| 命令面板首帧 | <12ms",
 "| 对话框无法关闭（bug）",
 "| 提示堆积 | 最多 3 条",
 "| `ui.toast.duration_ms`",
 "| `ui.focus.show_ring_on_mouse`",
 "| 17 持久化 |",
 "| `e2e/jb_recover` |",
], "p2")
p2 = repl(p2, [
 ("| 22 | 全量错误码→人话 | 每个 `DiagCode` 有三段式条目，覆盖率 100% 才发版 |",
  "| 17 / 22 | 恢复对话框语义、`OpenVerdict`；全量错误码→人话 | 永不自动删除 journal；每个 `DiagCode` 有三段式条目，覆盖率 100% 才发版 |"),
 ("| `e2e/jb_edit_undo_save` | J-B | 3 字节 → 补丁 +1；撤销归零；保存后 `已保存`；注入外部改动出现三按钮横幅 |",
  "| `e2e/jb_edit_undo_save` | J-B | 3 字节 → 补丁 +1；撤销归零；保存后 `已保存`；外部改动出现三按钮横幅；孤儿 journal 恢复行数与「丢弃进 .trash」正确 |"),
 ("每条脚本必须跑故障注入变体：只读挂载、磁盘满、断网、权限拒绝、进程退出；断言失败必须能定位到 `TextKey`。\n\n", ""),
 ("- 播报粒度：游标只在「行变化」或「停止输入 300ms」后播报；选区只播报 `选区 N 字节，起 0x…，止 0x…`；无变化不播报。结构树节点播报 `字段 name，偏移 0x1A3C，长度 4，值 3，置信 0.82 结构自洽`。",
  "- 播报粒度：游标只在「行变化」或「停止输入 300ms」后播报；选区只播报 `选区 N 字节，起 0x…，止 0x…`；无变化不播报。结构树节点播报 `字段 name，偏移 0x1A3C，长度 4，值 3，置信 0.82`。"),
], "p2")
wr("part2b.md", p2)
print("part2b", len(p2))
