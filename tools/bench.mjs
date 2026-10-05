// v1（Python）vs v2（Rust）基准：exe 体积、冷启动、空闲/峰值内存、回环传输吞吐、WASM 加解密速度。
//
// 用法：node tools/bench.mjs --v1 <v1 的 LanShare.exe> --v2 <v2 的 LanShare.exe> [--mib 256] [--runs 5] [--out bench.json]
//
// - 冷启动：启动进程 → info 文件写出 → GET / 返回 200 的耗时（每轮新临时目录，先跑一轮预热不计）
// - 内存：进程树（v1 的 PyInstaller onefile 是“引导进程 + Python 子进程”两个）的工作集之和
// - 吞吐：本机回环。v1 是明文 HTTP 整文件 PUT/GET；v2 是浏览器端同一套 proto.js + WASM，分块加密、3 块并发
//   两边都校验 SHA-256 一致；文件都写进临时目录（同一块 SSD）
import { spawn, execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, existsSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { webcrypto, createHash, randomBytes } from "node:crypto";
import { loadCrypto, LanShareClient, keyFromFragment } from "../crates/lanshare/web/proto.js";

const args = Object.fromEntries(process.argv.slice(2).reduce((acc, a, i, all) => (a.startsWith("--") ? [...acc, [a.slice(2), all[i + 1]]] : acc), []));
const MIB = Number(args.mib ?? 256);
const RUNS = Number(args.runs ?? 5);
const ROOT = new URL("..", import.meta.url);
const WASM = readFileSync(new URL("crates/lanshare/web/lanshare.wasm", ROOT));
const sha = (b) => createHash("sha256").update(b).digest("hex");
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const median = (xs) => [...xs].sort((a, b) => a - b)[Math.floor(xs.length / 2)];

class MemoryStorage {
  #m = new Map();
  getItem(k) { return this.#m.has(k) ? this.#m.get(k) : null; }
  setItem(k, v) { this.#m.set(k, String(v)); }
  removeItem(k) { this.#m.delete(k); }
}

/** 启动 exe，返回 { proc, info, readyMs, work }。 */
async function launch(exe) {
  const work = mkdtempSync(join(tmpdir(), "lanshare-bench-"));
  const infoPath = join(work, "info.json");
  const t0 = performance.now();
  const proc = spawn(exe, ["--no-tray", "--no-browser", "--port", "0", "--dir", join(work, "share"), "--info-file", infoPath],
    { stdio: "ignore", env: { ...process.env, LOCALAPPDATA: work } });
  let info;
  for (;;) {
    if (performance.now() - t0 > 30000) throw new Error(`${exe} 30 秒内没有就绪`);
    if (existsSync(infoPath)) {
      try { info = JSON.parse(readFileSync(infoPath, "utf8")); } catch { /* 还没写完 */ }
      if (info) {
        try { if ((await fetch(`http://127.0.0.1:${info.port}/`)).ok) break; } catch { /* 还没开始监听 */ }
      }
    }
    await sleep(5);
  }
  return { proc, info, readyMs: performance.now() - t0, work, base: `http://127.0.0.1:${info.port}` };
}

function stop({ proc, work }) {
  try { execFileSync("taskkill", ["/PID", String(proc.pid), "/T", "/F"], { stdio: "ignore" }); } catch { /* 已经退出 */ }
  for (let i = 0; i < 20; i++) {
    try { rmSync(work, { recursive: true, force: true }); return; } catch { Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 200); } // 进程刚被杀，文件句柄可能还没释放
  }
}

/** 进程树的 [工作集, 峰值工作集] 之和（MiB）。 */
function memoryMiB(pid) {
  const ps = `$ids = @(${pid}) + @(Get-CimInstance Win32_Process -Filter 'ParentProcessId=${pid}' | ForEach-Object ProcessId);` +
    `$p = Get-Process -Id $ids -ErrorAction SilentlyContinue;` +
    `'{0} {1}' -f ($p | Measure-Object WorkingSet64 -Sum).Sum, ($p | Measure-Object PeakWorkingSet64 -Sum).Sum`;
  const [ws, peak] = execFileSync("powershell", ["-NoProfile", "-Command", ps], { encoding: "utf8" }).trim().split(" ").map(Number);
  return { ws: ws / 2 ** 20, peak: peak / 2 ** 20 };
}

async function transferV1(base, pin, data, name) {
  const login = await fetch(`${base}/api/login`, { method: "POST", body: JSON.stringify({ pin }), headers: { "Content-Type": "application/json" } });
  const cookie = login.headers.get("set-cookie").split(";")[0];
  const path = `${base}/api/files/${encodeURIComponent(name)}`;
  let t = performance.now();
  const up = await fetch(path, { method: "PUT", body: data, headers: { Cookie: cookie } });
  if (up.status !== 201) throw new Error(`v1 上传失败 ${up.status}`);
  const upMs = performance.now() - t;
  t = performance.now();
  const back = Buffer.from(await (await fetch(path, { headers: { Cookie: cookie } })).arrayBuffer());
  const downMs = performance.now() - t;
  return { upMs, downMs, ok: sha(back) === sha(data) };
}

async function transferV2(base, keyB64, crypto, data, name) {
  const client = new LanShareClient(crypto, { base, storage: new MemoryStorage() });
  await client.serverId();
  await client.pairWithKey(keyFromFragment("#" + keyB64));
  let t = performance.now();
  const saved = await client.uploadFile(new File([data], name));
  const upMs = performance.now() - t;
  const entry = (await client.rpc({ op: "list" })).files.find((f) => f.name === saved);
  t = performance.now();
  const back = Buffer.from(await (await client.downloadFile(entry)).arrayBuffer());
  const downMs = performance.now() - t;
  return { upMs, downMs, ok: sha(back) === sha(data) };
}

function wasmSpeed(crypto) {
  const k = new Uint8Array(32).fill(7);
  const aad = new TextEncoder().encode("lanshare/2 req /api/up/x/0");
  const plain = new Uint8Array(1 << 20).fill(0x5a);
  for (let i = 0; i < 20; i++) crypto.open(k, 1n, aad, crypto.seal(k, 1n, aad, plain)); // 预热
  const N = 256;
  let t = performance.now();
  const sealed = [];
  for (let i = 0; i < N; i++) sealed.push(crypto.seal(k, BigInt(i + 1), aad, plain));
  const seal = N / ((performance.now() - t) / 1000);
  t = performance.now();
  for (let i = 0; i < N; i++) crypto.open(k, BigInt(i + 1), aad, sealed[i]);
  const open = N / ((performance.now() - t) / 1000);
  return { seal, open };
}

async function benchOne(label, exe, crypto, data) {
  const result = { label, exe, bytes: statSync(exe).size, startMs: [], idle: null, transfer: [] };
  stop(await launch(exe)); // 预热：第一次启动可能要冷读磁盘
  for (let i = 0; i < RUNS; i++) {
    const run = await launch(exe);
    result.startMs.push(run.readyMs);
    if (i === 0) {
      await sleep(3000);
      result.idle = memoryMiB(run.proc.pid);
    }
    stop(run);
  }
  for (let i = 0; i < 3; i++) {
    const run = await launch(exe);
    const name = `bench-${i}.bin`;
    const r = label === "v1"
      ? await transferV1(run.base, run.info.pin, data, name)
      : await transferV2(run.base, run.info.key, crypto, data, name);
    if (!r.ok) throw new Error(`${label} 传输后内容不一致`);
    if (i === 0) result.afterTransfer = memoryMiB(run.proc.pid);
    result.transfer.push(r);
    stop(run);
  }
  return result;
}

const crypto = await loadCrypto(WASM, (b) => webcrypto.getRandomValues(b));
const data = randomBytes(MIB * 2 ** 20);
const out = { date: new Date().toISOString(), node: process.version, mib: MIB, runs: RUNS, wasm: wasmSpeed(crypto), results: [] };
for (const [label, exe] of [["v1", args.v1], ["v2", args.v2]]) {
  if (!exe) continue;
  process.stderr.write(`测 ${label}：${exe}\n`);
  out.results.push(await benchOne(label, exe, crypto, data));
}

const fmt = (n, d = 0) => n.toFixed(d);
console.log(`| 指标 | ${out.results.map((r) => r.label).join(" | ")} |`);
console.log(`|---|${out.results.map(() => "---").join("|")}|`);
console.log(`| exe 体积 | ${out.results.map((r) => `${fmt(r.bytes / 2 ** 20, 1)} MiB`).join(" | ")} |`);
console.log(`| 冷启动到可访问（中位数，${RUNS} 次） | ${out.results.map((r) => `${fmt(median(r.startMs))} ms（${fmt(Math.min(...r.startMs))}–${fmt(Math.max(...r.startMs))}）`).join(" | ")} |`);
console.log(`| 空闲内存（工作集） | ${out.results.map((r) => `${fmt(r.idle.ws, 1)} MiB`).join(" | ")} |`);
console.log(`| 传完 ${MIB} MiB 后的峰值内存 | ${out.results.map((r) => `${fmt(r.afterTransfer.peak, 1)} MiB`).join(" | ")} |`);
const mibs = (ms) => MIB / (ms / 1000);
console.log(`| 回环上传（中位数，3 次） | ${out.results.map((r) => `${fmt(median(r.transfer.map((t) => mibs(t.upMs))))} MiB/s`).join(" | ")} |`);
console.log(`| 回环下载（中位数，3 次） | ${out.results.map((r) => `${fmt(median(r.transfer.map((t) => mibs(t.downMs))))} MiB/s`).join(" | ")} |`);
console.log(`\nWASM（Node ${process.version}）：加密 ${fmt(out.wasm.seal)} MiB/s，解密 ${fmt(out.wasm.open)} MiB/s`);
if (args.out) writeFileSync(args.out, JSON.stringify(out, null, 2));
