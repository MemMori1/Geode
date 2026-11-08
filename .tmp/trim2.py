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
            hit.add(m[0]); continue
        out.append(l)
    miss = [s for s in subs if s not in hit]
    if miss:
        print("MISS-drop", tag, miss)
    return out

def repl(ls, pairs, tag):
    txt = "\n".join(ls)
    for a, b in pairs:
        if a not in txt:
            print("MISS-repl", tag, a.split("\n")[0][:60])
        txt = txt.replace(a, b, 1)
    return txt.split("\n")

# part1
p1 = rd("part1.md")
p1 = drop(p1, [
 "pub struct TextKey(pub &'static str);",
 "impl DocState { pub fn caps(self) -> StateCaps; }",
 "| 取消 | p99 ≤100ms，500ms 内显示「已取消」",
], "p1")
p1 = repl(p1, [
 ("pub struct UiText {", "// TextKey = i18n key，命名空间见 16 §4.6；所有占位符见 args\npub struct UiText {"),
], "p1")
wr("part1.md", p1)
print("part1", len(p1))

# part2b
p2 = rd("part2b.md")
p2 = drop(p2, [
 "| `ui.copy.locale_fallback`",
 "| `ui.a11y.announce_throttle_ms`",
 "| 只读源上的编辑意图",
 "| i18n key 缺失 |",
 "| 跳跃/滚动 | 浮层 ≤100ms",
 "| 键入到上屏 | ≤16ms",
], "p2")
p2 = repl(p2, [
 ("| BinSpec 解析节点 | 结构 | `Node`(BinSpec) / `struct` | 结构体、类型 |\n| 结构内的命名值 | 字段 | `field` | 属性、成员 |",
  "| BinSpec 解析节点 / 节点内的命名值 | 结构 / 字段 | `Node` / `field` | 结构体、类型、属性、成员 |"),
 ("3. 长任务永不阻塞输入且可取消：>300ms 一律进 `Job`，取消 p99 ≤100ms；取消后搜索保留部分结果并标 `complete=false`，diff 则丢弃半份（`09 §6`）。\n4. 进度分级：<300ms 不显示；300ms~2s 骨架或不确定态；>2s 进度条 + 百分比 + 取消；30s 无进度事件必须输出「仍在处理」。",
  "3. 长任务永不阻塞输入且可取消：>300ms 一律进 `Job`，取消 p99 ≤100ms；取消后搜索保留部分结果并标 `complete=false`，diff 则丢弃半份（`09 §6`）。进度分级：<300ms 不显示，300ms~2s 骨架，>2s 进度条 + 取消，30s 无事件报「仍在处理」。"),
 ("| 05 hex 视图 | 滚动与跳跃预览、编辑输入与撤销单元、`reveal_range`/`deepest_at` | 非法输入不静默丢；联动未命中不改变树 |\n| 08 搜索 | 分批上屏、`base_version`、`NonContiguous` | 结果始终带版本号；过期不自动重跑 |",
  "| 05 / 08 | 滚动与跳跃预览、编辑与撤销单元、`reveal_range`/`deepest_at`；分批上屏、`base_version`、`NonContiguous` | 非法输入不静默丢、联动未命中不改树；结果带版本号、过期不自动重跑 |"),
 ("- 最小点击目标 24dp（图标按钮与行高同规则），分隔条命中区 6dp；对比度正文 4.5:1、控件 3:1、高对比 7:1（`16 §9`）。",
  "- 最小点击目标 24dp、分隔条命中区 6dp；对比度正文 4.5:1 / 控件 3:1 / 高对比 7:1（`16 §9`）。"),
 ("- 样本 5 人（含 1 名仅键盘用户、1 名屏幕阅读器用户）× 3 任务：① 打开 20GB 跳到 60% 读一个值；② 改 3 字节、撤销、走一次冲突分支；③ 无 magic 命中时用「AI 猜结构」。判据：完成 ≥4/5、首次求助前无卡住表述、错误路径下能复述「发生了什么 + 下一步」。\n- 每个里程碑做一次错误路径人工演练（非作者执行）：拔盘、权限拒绝、磁盘写满、签名篡改包、强杀进程后恢复、只读源上编辑、主题 token 缺失、插件崩溃；记录三段是否齐全、出口动作是否有效、是否误弹模态。",
  "- 样本 5 人（含 1 名仅键盘用户、1 名屏幕阅读器用户）× 3 任务：① 打开 20GB 跳到 60% 读一个值；② 改 3 字节、撤销、走一次冲突分支；③ 无 magic 命中时用「AI 猜结构」。判据：完成 ≥4/5、首次求助前无卡住表述、错误路径下能复述「发生了什么 + 下一步」。\n- 每个里程碑做一次错误路径人工演练（非作者执行）：拔盘、权限拒绝、磁盘写满、签名篡改包、强杀进程后恢复、只读源上编辑、主题 token 缺失、插件崩溃；记录三段是否齐全、出口是否有效、是否误弹模态。"),
], "p2")
wr("part2b.md", p2)
print("part2b", len(p2))
