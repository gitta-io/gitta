"""Builds docs/_site/*.html from docs/pages/*.html. The published copy lives at https://gitta.io/docs.

Each page starts with a comment holding its metadata:
    <!-- title: Install | group: Get started | order: 2 | desc: One line for search results | seo: optional page title for search results -->
Terminal blocks are written as <term title="...">...</term>; the text inside is plain text (escaped here), and lines
starting with "$ " get a prompt. Values that are not decided yet live in SITE below, so they change in one place.

    python3 docs/build.py
"""
import html, json, pathlib, re

BASE = "https://gitta.io"                                              # public address of the site
SITE = {
    "RELEASES": "https://github.com/gitta-io/gitta/releases",       # where releases and install.sh will live
    "ACTION": "gitta-io/gitta/gitta-action@v1",                      # the GitHub Action
    "SUPPORT": "support@gitta.io",                                   # support address
    "VERSION": "0.1.0",
    "INSTALL": "https://gitta.io/install.sh",                        # install script, counted by the gitta.io worker
    "SALES": "hello@gitta.io",
}
GROUPS = ["Get started", "Using gitta", "Help"]

root = pathlib.Path(__file__).resolve().parent
out = root / "_site"
out.mkdir(exist_ok=True)

pages = []
for f in sorted((root / "pages").glob("*.html")):
    text = f.read_text()
    meta = dict(kv.split(":", 1) for kv in re.match(r"<!--(.*?)-->", text, re.S).group(1).split("|"))
    meta = {k.strip(): v.strip() for k, v in meta.items()}
    pages.append({"file": f.name, "title": meta["title"], "group": meta["group"], "order": float(meta["order"]),
                  "desc": meta.get("desc", ""), "seo": meta.get("seo", meta["title"]), "body": text[text.index("-->") + 3:]})
pages.sort(key=lambda p: (GROUPS.index(p["group"]), p["order"]))


def term_colours(e):
    """Colour escaped terminal text: `symbols`, failures, successes, flags."""
    e = re.sub(r"`([^`]+)`", r'<span class="y">`\1`</span>', e)
    e = re.sub(r"(?<![\w-])(--?[a-z][\w-]*)", r'<span class="b">\1</span>', e)
    e = re.sub(r"\b(refused|FAILED|FAIL|Error|error|HIGH|AttributeError|failed)\b", r'<span class="r">\1</span>', e)
    e = re.sub(r"\b(landed|ok|PASS|passed|valid)\b", r'<span class="g">\1</span>', e)
    return e


def term(m):
    title = m.group(1) or "Terminal"
    lines = []
    for line in html.unescape(m.group(2)).strip("\n").split("\n"):   # entities written in the source show as their characters
        e = html.escape(line, quote=False)
        e = term_colours(e)
        if line.startswith("$ "):
            e = '<span class="p">$</span> ' + re.sub(r"^(\S+)", r'<span class="g">\1</span>', term_colours(html.escape(line[2:], quote=False)))
        elif line.lstrip().startswith("#"):
            e = '<span class="c">' + e + "</span>"
        lines.append(e)
    return (f'<div class="term"><div class="bar"><i></i><i></i><i></i>&nbsp;{html.escape(title)}</div>'
            f'<pre><code>{chr(10).join(lines)}</code></pre></div>')


KW = {
    "py": "def return if else elif for in import from as class None True False not and or with try except raise lambda yield async await pass",
    "ts": "import export type const let var function return if else new from as interface extends async await true false null undefined",
    "go": "func return if else for range package import type struct var const nil true false go defer chan map interface",
    "yaml": "true false null",
    "sh": "",
}


def code_lang(text):
    first = text.lstrip(" \n").split("\n", 1)[0]
    for ext, lang in ((".yml", "yaml"), (".yaml", "yaml"), (".py", "py"), (".ts", "ts"), (".go", "go"), (".toml", "yaml"), (".json", "ts")):
        if ext in first:
            return lang
    if re.search(r"^\s*[\w.-]+:\s", text, re.M):
        return "yaml"
    return "sh"


def hl_line(line, lang):
    """Colour one line: comments, strings, keywords, numbers, YAML keys, function calls."""
    cmt = r"#.*" if lang in ("py", "yaml", "sh") else r"//.*|/\*.*?\*/|^\s*\*.*"
    tok = re.compile(rf"(?P<c>{cmt})|(?P<s>\"(?:[^\"\\]|\\.)*\"|'(?:[^'\\]|\\.)*'|`[^`]*`|\$\{{\{{.*?\}}\}})"
                     r"|(?P<k>^\s*-?\s*[\w./-]+(?=:(\s|$)))|(?P<n>\b\d[\d_.]*\b)|(?P<f>\b[A-Za-z_]\w*(?=\())|(?P<w>\b[A-Za-z_]\w*\b)")
    kws = set(KW[lang].split())
    out, i = [], 0
    for m in tok.finditer(line):
        out.append(html.escape(line[i:m.start()], quote=False))
        t, kind = m.group(0), m.lastgroup
        cls = {"c": "tc", "s": "ts", "n": "tn", "f": "tf"}.get(kind)
        if kind == "k":
            cls = "tk" if lang == "yaml" else None
        if kind == "w":
            cls = "tw" if t in kws else None
        e = html.escape(t, quote=False)
        out.append(f'<span class="{cls}">{e}</span>' if cls else e)
        i = m.end()
    out.append(html.escape(line[i:], quote=False))
    return "".join(out)


def highlight(m):
    text = html.unescape(m.group(1))
    lang = code_lang(text)
    diff = text.startswith("  ") and re.search(r"^[+-]", text, re.M)
    lines = []
    for line in text.split("\n"):
        if diff and line[:1] in "+-" and line:
            lines.append(f'<span class="d{"a" if line[0] == "+" else "r"}">{html.escape(line[0])}{hl_line(line[1:], lang)}</span>')
        elif diff:
            lines.append(" " + hl_line(line[1:], lang) if line.startswith(" ") else hl_line(line, lang))
        else:
            lines.append(hl_line(line, lang))
    return f'<pre class="code lang-{lang}"><code>{chr(10).join(lines)}</code></pre>'




def nav(current):
    s = ""
    for g in GROUPS:
        s += f"<h4>{g}</h4>"
        for p in pages:
            if p["group"] == g:
                cur = ' aria-current="page"' if p["file"] == current else ""
                s += f'<a href="{p["file"]}"{cur}>{p["title"]}</a>'
    return s


HEAD = """<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title} | gitta docs</title><meta name="description" content="{desc}"><meta name="theme-color" content="#F7F7F2">
<link rel="canonical" href="{url}"><meta property="og:type" content="article"><meta property="og:site_name" content="gitta"><meta property="og:title" content="{title}"><meta property="og:description" content="{desc}"><meta property="og:url" content="{url}"><meta property="og:image" content="{base}/assets/og.png"><meta name="twitter:card" content="summary_large_image">
<script type="application/ld+json">{ld}</script>
<link rel="stylesheet" href="docs.css"></head>
<body><a class="skip" href="#doc">Skip to content</a>
<main class="wrap docs"><aside class="docs-nav" aria-label="Documentation">{nav}</aside><article class="doc" id="doc"><details class="docs-toc"><summary>Contents</summary><nav class="docs-nav" aria-label="Documentation">{nav}</nav></details>"""
FOOT = """{pager}</article></main>
<footer class="footer"><div class="wrap"><div class="footer-top"><div class="footer-tagline">More progress. Less repair.</div><div class="footer-links"><a href="index.html">Docs</a><a href="support.html">Support</a><a href="https://gitta.io">gitta.io</a><a href="https://gitta.io/privacy.html">Privacy</a></div></div><div class="footer-line"><p>gitta © 2026</p></div></div></footer>
</body></html>"""

for i, p in enumerate(pages):
    body = p["body"]
    for k, v in SITE.items():
        body = body.replace("{{" + k + "}}", v)
    body = re.sub(r'<term(?: title="([^"]*)")?>(.*?)</term>', term, body, flags=re.S)
    body = re.sub(r'<pre class="code"><code>(.*?)</code></pre>', highlight, body, flags=re.S)
    def anchor(m):
        slug = re.sub(r"[^a-z0-9]+", "-", re.sub(r"<[^>]+>", "", m.group(1)).lower()).strip("-")
        return f'<h2 id="{slug}">{m.group(1)}</h2>'
    body = re.sub(r"<h2>(.*?)</h2>", anchor, body)
    prev = pages[i - 1] if i > 0 else None
    nxt = pages[i + 1] if i + 1 < len(pages) else None
    pager = '<nav class="pager" aria-label="Pages">' + \
        (f'<a href="{prev["file"]}"><span>Previous</span>{prev["title"]}</a>' if prev else "<span></span>") + \
        (f'<a href="{nxt["file"]}" style="text-align:right"><span>Next</span>{nxt["title"]}</a>' if nxt else "<span></span>") + "</nav>"
    url = f"{BASE}/docs/{p['file']}"
    ld = json.dumps({"@context": "https://schema.org", "@graph": [
        {"@type": "TechArticle", "headline": p["seo"], "description": p["desc"], "url": url, "inLanguage": "en",
         "publisher": {"@type": "Organization", "name": "Gitta", "url": BASE}, "about": "Gitta, version control for AI coding agents"},
        {"@type": "BreadcrumbList", "itemListElement": [
            {"@type": "ListItem", "position": 1, "name": "Docs", "item": f"{BASE}/docs/"},
            {"@type": "ListItem", "position": 2, "name": p["group"], "item": f"{BASE}/docs/"},
            {"@type": "ListItem", "position": 3, "name": p["title"], "item": url}]}]}).replace("</", "<\\/")
    cur = ' aria-current="page"'
    top = "how" if p["file"] == "how-it-works.html" else "ent" if p["group"] == "Gitta Enterprise" else "docs"
    page = HEAD.format(title=html.escape(p["seo"]), desc=html.escape(p["desc"]), nav=nav(p["file"]), url=url, base=BASE, ld=ld,
                       cur_how=cur if top == "how" else "", cur_docs=cur if top == "docs" else "", cur_ent=cur if top == "ent" else "") + body + FOOT.format(pager=pager)
    left = re.findall(r"\{\{\w+\}\}", page)
    assert not left, f"{p['file']}: unknown value {left}"
    (out / p["file"]).write_text(page)

(out / "docs.css").write_text((root / "docs.css").read_text())
print(f"{len(pages)} pages written to {out}")
