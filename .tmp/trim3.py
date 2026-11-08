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

p1 = rd("part1.md")
p1 = repl(p1, [
 ("| D7 | 自绘网格走「可访问镜像」，播报粒度为光标行与选区首尾行 | 逐格播报等于噪声 |\n| D8 | 快捷键与菜单文字从命令注册表现取，文案不写字面量 `Ctrl+G` | 键位可重绑，文案不能腐化 |",
  "| D7 | 自绘网格走可访问镜像（播报粒度为游标行与选区首尾行）；键位与菜单文字从命令注册表现取 | 逐格播报是噪声；键位可重绑，文案不能腐化 |"),
], "p1")
wr("part1.md", p1)
print("part1", len(p1))

p2 = rd("part2b.md")
p2 = drop(p2, [
 "| 取消 | p99 ≤100ms",
 "| AI/解析首段 | 300ms 候选",
 "| 非法状态转移 |",
 "| `ui.banner.max_visible`",
], "p2")
p2 = repl(p2, [
 ("4. **是否提供「专家模式」隐藏解释性文案**：`ui.copy.expert_mode` 已预留，但「为什么」正是新手最需要的部分，需用数据决定。\n5. **术语「游标/光标」统一与线框反向链接**：本文定为「游标」，05/16 正文仍有「光标」，下次修订须对齐；同时建议 16 §3.1 加一条「区域级画面见 25 §4.2」，避免两处描述漂移。",
  "4. **专家模式与术语统一**：`ui.copy.expert_mode` 预留了「隐藏为什么」，但「为什么」正是新手最需要的部分，需用数据决定；术语定为「游标」，05/16 正文仍写「光标」，下次修订须对齐，并建议 16 §3.1 加「区域级画面见 25 §4.2」避免两处漂移。"),
 ("- 色盲不依赖色相：Error/Warning/Info 带 `✕ ! i` 前缀；AI 置信带 `✓ ≈ ?` 徽标 + 条形长度；diff 用底色 + 左缘 3px 竖条 + 行内 `+`/`-`；补丁行用底色 + 行首标记。\n- 纯键盘全流程：六条旅程各有键盘脚本（§9.2），可用性测试必须含一轮「不碰鼠标」；高 DPI 下文案不得依赖像素截断，省略号必配 tooltip，伪本地化 +30% 后零截断。",
  "- 色盲不依赖色相：Error/Warning/Info 带 `✕ ! i` 前缀；AI 置信带 `✓ ≈ ?` 徽标 + 条形长度；diff 用底色 + 左缘 3px 竖条 + 行内 `+`/`-`；补丁行用底色 + 行首标记。\n- 纯键盘全流程（§9.2 每旅程都有键盘脚本，可用性测试必含一轮「不碰鼠标」）；高 DPI 下文案不依赖像素截断，省略号必配 tooltip，伪本地化 +30% 零截断。"),
 ("### 4.3 全局文档状态机\n\n```", "### 4.3 全局文档状态机\n```"),
 ("### 4.4 文案规范\n\n**语气**", "### 4.4 文案规范\n**语气**"),
], "p2")
wr("part2b.md", p2)
print("part2b", len(p2))
