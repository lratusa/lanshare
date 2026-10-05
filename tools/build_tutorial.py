"""把 docs/learn-rust/*.md 生成为一个可交互的教程网页：docs/learn-rust/site/index.html。

- 构建时用 Python-Markdown 渲染、Pygments 高亮，页面不加载任何外部脚本
- 每章的“小测验”变成可作答的卡片：先写答案，再看解析，自评“掌握了 / 还没掌握”，进度存在浏览器本地
- 章节之间的链接改成页内跳转（#ch00 这样的锚点）；图片和附件用相对路径，发布时一起上传

用法：python tools/build_tutorial.py
"""
import html
import re
import sys
from pathlib import Path

import markdown
from pygments import highlight
from pygments.formatters import HtmlFormatter
from pygments.lexers import get_lexer_by_name

sys.stdout.reconfigure(encoding="utf-8")

ROOT = Path(__file__).resolve().parent.parent
DOCS = ROOT / "docs" / "learn-rust"
OUT = DOCS / "site" / "index.html"

LANG_LABEL = {"rust": "Rust", "python": "Python", "js": "JavaScript", "toml": "TOML",
              "powershell": "PowerShell", "bash": "终端", "": ""}
LEXER = {"js": "javascript"}
FENCE = re.compile(r"^```([a-z]*)\n(.*?)^```\s*$", re.S | re.M)


def render_code(lang, code):
    """一段代码 → 带语言标签的代码面板。编译器报错输出单独标红。"""
    looks_like_error = lang == "" and re.match(r"\s*(error|warning)(\[E\d+\])?:", code) is not None
    if lang:
        body = highlight(code, get_lexer_by_name(LEXER.get(lang, lang)), HtmlFormatter(nowrap=True))
    else:
        body = html.escape(code)
    label = "编译器报错" if looks_like_error else LANG_LABEL.get(lang, lang)
    cls = "code is-error" if looks_like_error else "code"
    head = f'<figcaption>{label}</figcaption>' if label else ""
    return f'<figure class="{cls}">{head}<pre><code>{body.rstrip()}</code></pre></figure>'


def to_html(md_text):
    """Markdown → HTML。代码块先抽出来自己高亮，再放回去。"""
    blocks = []

    def stash(m):
        blocks.append(render_code(m.group(1), m.group(2)))
        return f"\n\nCODEBLOCK{len(blocks) - 1}END\n\n"

    text = FENCE.sub(stash, md_text)
    out = markdown.markdown(text, extensions=["tables", "sane_lists"])
    out = re.sub(r"<p>CODEBLOCK(\d+)END</p>", lambda m: blocks[int(m.group(1))], out)
    out = re.sub(r"CODEBLOCK(\d+)END", lambda m: blocks[int(m.group(1))], out)
    # 章节互链改成页内锚点
    out = re.sub(r'href="(\d\d)-[^"]+\.md"', r'href="#ch\1"', out)
    out = out.replace("<table>", '<div class="table"><table>').replace("</table>", "</table></div>")
    out = out.replace("<img ", '<img loading="lazy" ')
    return out


def split_sections(body):
    """按二级标题切开：[(标题, markdown), ...]；第一个元素的标题为空（章首导语）。"""
    parts = re.split(r"^## (.+)$", body, flags=re.M)
    sections = [("", parts[0])]
    for i in range(1, len(parts), 2):
        sections.append((parts[i].strip(), parts[i + 1]))
    return sections


QUIZ_ITEM = re.compile(r"^\*\*(\d+)\.\s*(.+?)\*\*\s*$(.*?)<details><summary>答案</summary>(.*?)</details>", re.S | re.M)


def render_quiz(chapter_id, md_text):
    items = []
    for m in QUIZ_ITEM.finditer(md_text):
        num, title, extra, answer = m.group(1), m.group(2), m.group(3), m.group(4)
        qid = f"{chapter_id}-q{num}"
        question = to_html(f"**{title}**\n\n{extra.strip()}") if extra.strip() else f"<p><strong>{to_inline(title)}</strong></p>"
        items.append(f"""
<article class="quiz" data-q="{qid}">
  <header><span class="quiz-no">第 {num} 题</span><span class="quiz-state" aria-live="polite"></span></header>
  <div class="quiz-q">{question}</div>
  <label class="sr" for="{qid}-a">你的答案</label>
  <textarea id="{qid}-a" rows="3" placeholder="先写下你的想法，再看解析（只保存在你自己的浏览器里）"></textarea>
  <div class="quiz-actions">
    <button type="button" class="btn reveal">看解析</button>
  </div>
  <div class="quiz-answer" hidden>
    <div class="answer-label">解析</div>
    {to_html(answer.strip())}
    <div class="grade">
      <span>对照解析，你：</span>
      <button type="button" class="btn grade-yes">掌握了</button>
      <button type="button" class="btn ghost grade-no">还没掌握</button>
    </div>
  </div>
</article>""")
    return "\n".join(items), len(items)


def to_inline(md_text):
    out = markdown.markdown(md_text)
    return re.sub(r"^<p>|</p>$", "", out.strip())


def build_chapter(path):
    num = path.name[:2]
    cid = f"ch{num}"
    text = path.read_text(encoding="utf-8")
    title_line, body = text.split("\n", 1)
    title = title_line.lstrip("# ").strip()
    parts, quiz_count = [], 0
    for heading, content in split_sections(body):
        if heading == "小测验":
            quiz_html, quiz_count = render_quiz(cid, content)
            parts.append(f'<section class="sec sec-quiz"><h2 id="{cid}-quiz">小测验</h2>'
                         f'<p class="quiz-intro">先自己想，写下答案，再看解析。</p>{quiz_html}</section>')
        elif heading:
            cls = "sec sec-py" if "Python" in heading else "sec"
            tag = '<span class="py-tag">PY</span>' if "Python" in heading else ""
            parts.append(f'<section class="{cls}"><h2>{tag}{to_inline(heading)}</h2>{to_html(content)}</section>')
        elif content.strip():
            parts.append(f'<section class="sec lead">{to_html(content)}</section>')
    # “第 7 章　标题”拆成章号和标题
    m = re.match(r"第\s*(\d+)\s*章\s*(.+)", title)
    kicker, name = (f"第 {int(m.group(1))} 章", m.group(2)) if m else ("", title)
    short = re.split(r"[：:]", name)[0]
    article = (f'<article class="chapter" id="{cid}" data-quiz="{quiz_count}" hidden>'
               f'<header class="chapter-head"><p class="kicker">{kicker}</p><h1>{html.escape(name)}</h1></header>'
               f'{"".join(parts)}<nav class="pager"></nav></article>')
    return {"id": cid, "kicker": kicker, "short": short, "quiz": quiz_count, "html": article}


def build_home(chapters):
    readme = (DOCS / "README.md").read_text(encoding="utf-8")
    body = readme.split("\n", 1)[1]
    body = re.sub(r"^## 章节.*?(?=^## |\Z)", "", body, flags=re.M | re.S)  # 目录用下面生成的卡片
    body = body.replace("（章节随开发进度陆续补全。）", "")
    cards = "".join(
        f'<a class="toc-item" href="#{c["id"]}"><span class="toc-no">{c["kicker"] or "速查"}</span>'
        f'<span class="toc-title">{html.escape(c["short"])}</span>'
        f'<span class="toc-progress" data-for="{c["id"]}">{c["quiz"]} 题</span></a>'
        for c in chapters)
    return (f'<article class="chapter home" id="home" hidden>'
            f'<header class="chapter-head"><p class="kicker">写给会 Python 的人</p><h1>跟着 LanShare 学 Rust</h1></header>'
            f'<section class="sec lead">{to_html(body)}</section>'
            f'<section class="sec"><h2>章节</h2><div class="toc">{cards}</div></section></article>')


def main():
    paths = sorted(p for p in DOCS.glob("[0-9][0-9]-*.md"))
    chapters = [build_chapter(p) for p in paths]
    template = (ROOT / "tools" / "tutorial_template.html").read_text(encoding="utf-8")
    nav = "".join(
        f'<a href="#{c["id"]}" data-for="{c["id"]}"><span class="nav-no">{c["id"][2:]}</span>'
        f'<span class="nav-title">{html.escape(c["short"])}</span><span class="nav-progress"></span></a>'
        for c in chapters)
    page = (template.replace("{{NAV}}", nav)
            .replace("{{OPTIONS}}", "".join(f'<option value="{c["id"]}">{c["id"][2:]} · {html.escape(c["short"])}</option>' for c in chapters))
            .replace("{{CHAPTERS}}", build_home(chapters) + "".join(c["html"] for c in chapters))
            .replace("{{TOTAL}}", str(sum(c["quiz"] for c in chapters))))
    OUT.parent.mkdir(exist_ok=True)
    OUT.write_text(page, encoding="utf-8")
    print(f"{OUT}: {len(chapters)} 章，{sum(c['quiz'] for c in chapters)} 道题，{len(page) // 1024} KB")


if __name__ == "__main__":
    main()
