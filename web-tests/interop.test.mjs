// 跨实现互通：浏览器端的 proto.js + 真实 lanshare.wasm，对真实启动的 LanShare.exe。
// 运行前：cargo build -p lanshare（debug 即可）。环境变量 LANSHARE_EXE 可指定别的 exe（build.ps1 用它冒烟 release 版）
import { test, before, after } from "node:test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdtempSync, readFileSync, existsSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { webcrypto, createHash } from "node:crypto";
import { fileURLToPath } from "node:url";
import { loadCrypto, LanShareClient, WrongPin, keyFromFragment } from "../crates/lanshare/web/proto.js";

const ROOT = new URL("..", import.meta.url);
const EXE = process.env.LANSHARE_EXE || fileURLToPath(new URL("target/debug/LanShare.exe", ROOT));
const WASM = new URL("crates/lanshare/web/lanshare.wasm", ROOT);
const sha = (buf) => createHash("sha256").update(buf).digest("hex");

class MemoryStorage {
  #m = new Map();
  getItem(k) { return this.#m.has(k) ? this.#m.get(k) : null; }
  setItem(k, v) { this.#m.set(k, String(v)); }
  removeItem(k) { this.#m.delete(k); }
}

let proc, info, base, crypto, work;

before(async () => {
  work = mkdtempSync(join(tmpdir(), "lanshare-interop-"));
  const infoPath = join(work, "info.json");
  proc = spawn(EXE, ["--no-tray", "--no-browser", "--port", "0", "--dir", join(work, "share"), "--info-file", infoPath],
    { stdio: "ignore", env: { ...process.env, LOCALAPPDATA: work } });
  for (let i = 0; i < 200 && !existsSync(infoPath); i++) await new Promise((r) => setTimeout(r, 50));
  info = JSON.parse(readFileSync(infoPath, "utf8"));
  base = `http://127.0.0.1:${info.port}`;
  crypto = await loadCrypto(readFileSync(WASM), (b) => webcrypto.getRandomValues(b));
});

after(() => {
  proc?.kill();
  try { rmSync(work, { recursive: true, force: true }); } catch {}
});

const newClient = (storage = new MemoryStorage()) => new LanShareClient(crypto, { base, storage });

test("exe 内嵌的网页资源与仓库里的文件一致（防止打包进旧的 WASM/页面）", async () => {
  for (const name of ["index.html", "app.js", "proto.js", "style.css", "lanshare.wasm"]) {
    const served = Buffer.from(await (await fetch(`${base}/${name === "index.html" ? "" : name}`)).arrayBuffer());
    const local = readFileSync(new URL(`crates/lanshare/web/${name}`, ROOT));
    assert.equal(sha(served), sha(local), name);
  }
});

test("二维码片段解析", () => {
  assert.equal(keyFromFragment("#" + info.key)?.length, 32);
  assert.equal(keyFromFragment(""), null);
  assert.equal(keyFromFragment("#abc"), null);
});

test("扫码配对 → 加密 RPC", async () => {
  const c = newClient();
  assert.equal(await c.serverId(), info.server_id);
  await c.pairWithKey(keyFromFragment("#" + info.key));
  const i = await c.rpc({ op: "info" });
  assert.equal(i.ok, true);
  assert.equal(i.pin, info.pin);
  assert.equal(i.local, true);
});

test("上传（分块并发）→ 列表 → 下载，内容一致，进度走满", async () => {
  const c = newClient();
  await c.pairWithKey(keyFromFragment("#" + info.key));
  const data = new Uint8Array(3 * 1024 * 1024 + 4321);
  webcrypto.getRandomValues(data.subarray(0, 65536));
  for (let off = 65536; off < data.length; off += 65536) data.set(data.subarray(0, Math.min(65536, data.length - off)), off);
  const progress = [];
  const saved = await c.uploadFile(new File([data], "互通 测试.bin"), { onProgress: (n) => progress.push(n) });
  assert.equal(saved, "互通 测试.bin");
  assert.equal(progress.at(-1), data.length);
  const f = (await c.rpc({ op: "list" })).files.find((x) => x.name === saved);
  assert.equal(f.size, data.length);
  const down = [];
  const blob = await c.downloadFile(f, { onProgress: (n) => down.push(n) });
  assert.equal(sha(Buffer.from(await blob.arrayBuffer())), sha(data));
  assert.equal(down.at(-1), data.length);
});

test("空文件与重名", async () => {
  const c = newClient();
  await c.pairWithKey(keyFromFragment("#" + info.key));
  assert.equal(await c.uploadFile(new File([], "空.txt")), "空.txt");
  assert.equal(await c.uploadFile(new File(["1"], "dup.txt")), "dup.txt");
  assert.equal(await c.uploadFile(new File(["2"], "dup.txt")), "dup (1).txt");
});

test("文字互发", async () => {
  const c = newClient();
  await c.pairWithKey(keyFromFragment("#" + info.key));
  assert.equal((await c.rpc({ op: "text_add", text: "来自 Node 的问候" })).ok, true);
  assert.equal((await c.rpc({ op: "texts" })).texts[0].text, "来自 Node 的问候");
});

test("口令配对（PAKE，JS/WASM ↔ Rust）后记住密钥，下次免输入", async () => {
  const storage = new MemoryStorage();
  const c = newClient(storage);
  await c.pairWithPin(info.pin);
  assert.equal((await c.rpc({ op: "info" })).ok, true);
  const again = newClient(storage);
  assert.equal(await again.resume(), true, "用存下的密钥重新握手");
  assert.equal((await again.rpc({ op: "texts" })).ok, true);
});

test("口令错误被识别", async () => {
  const wrong = info.pin === "000000" ? "111111" : "000000";
  await assert.rejects(newClient().pairWithPin(wrong), WrongPin);
});

test("没有存过密钥时 resume 返回 false", async () => {
  assert.equal(await newClient().resume(), false);
});

// 放在最后：会换口令，影响前面用 info.pin 的测试
test("本机界面重新开启口令登录：换新口令，新口令能用，旧口令作废", async () => {
  const local = newClient();
  await local.pairWithKey(keyFromFragment("#" + info.key));
  const before = await local.rpc({ op: "info" });
  assert.equal(before.pin_paused, false);
  const r = await local.rpc({ op: "pin_resume" });
  assert.equal(r.ok, true);
  assert.match(r.pin, /^\d{6}$/);
  assert.notEqual(r.pin, before.pin);
  await newClient().pairWithPin(r.pin);
  await assert.rejects(newClient().pairWithPin(before.pin), WrongPin);
});
