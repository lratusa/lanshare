// 页面静态检查：守住 CSP 与“不渲染任何 HTML 字符串”的约定。
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const web = (name) => readFileSync(new URL(`../crates/lanshare/web/${name}`, import.meta.url), "utf8");
const html = web("index.html");
const css = web("style.css");
const scripts = { "app.js": web("app.js"), "proto.js": web("proto.js") };

test("页面有全部关键元素", () => {
  for (const id of ["login", "loginForm", "pinInput", "loginMsg", "app", "picker", "queue", "fileList",
                    "textInput", "sendText", "textList", "qr", "pin", "url", "offline", "overlay", "toast"]) {
    assert.ok(html.includes(`id="${id}"`), id);
  }
});

test("app.js 里用到的元素，页面上都有", () => {
  const ids = [...scripts["app.js"].matchAll(/\$\("([A-Za-z]+)"\)/g)].map((m) => m[1]);
  assert.ok(ids.length > 20);
  for (const id of new Set(ids)) assert.ok(html.includes(`id="${id}"`), id);
});

test("没有内联脚本、内联样式、内联事件（CSP 会拦掉）", () => {
  const scriptTags = html.match(/<script\b[^>]*>[\s\S]*?<\/script>/g) || [];
  for (const tag of scriptTags) {
    assert.match(tag, /\bsrc="\/[a-z]+\.js"/, "脚本必须外链本站文件");
    assert.match(tag, /><\/script>$/, "script 标签里不能有内容");
  }
  assert.doesNotMatch(html, /\sstyle\s*=/i, "不能有 style 属性");
  assert.doesNotMatch(html, /\son[a-z]+\s*=/i, "不能有 onclick 之类的内联事件");
  assert.doesNotMatch(html, /<style\b/i, "不能有 <style> 块");
});

test("不引用任何外部资源（断网局域网也要能用）", () => {
  for (const [name, text] of [["index.html", html], ["style.css", css], ...Object.entries(scripts)]) {
    assert.doesNotMatch(text, /(src|href)\s*=\s*["']?(https?:)?\/\//i, name);
    assert.doesNotMatch(text, /@import|url\(\s*["']?(https?:)?\/\//i, name);
  }
});

test("从不把字符串当 HTML/代码执行", () => {
  for (const [name, text] of Object.entries(scripts)) {
    for (const sink of ["innerHTML", "outerHTML", "insertAdjacentHTML", "document.write", "eval(", "new Function"]) {
      assert.ok(!text.includes(sink), `${name} 里出现了 ${sink}`);
    }
  }
});

test("[hidden] 规则压过 display 设置", () => {
  assert.ok(css.includes("[hidden] { display: none !important; }"));
});
