// 浏览器端加密（lanshare.wasm + proto.js）与 OpenSSL 已知向量比对，并测速。
// 运行前先构建 wasm：cargo build -p lanshare-wasm --target wasm32-unknown-unknown --release
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { webcrypto } from "node:crypto";
import { loadCrypto } from "../crates/lanshare/web/proto.js";

const WASM = new URL("../target/wasm32-unknown-unknown/release/lanshare_wasm.wasm", import.meta.url);
const V = JSON.parse(readFileSync(new URL("../crates/lanshare-proto/tests/vectors.json", import.meta.url), "utf8"));
const hex = (u8) => Buffer.from(u8).toString("hex");
const unhex = (s) => new Uint8Array(Buffer.from(s, "hex"));
const enc = (s) => new TextEncoder().encode(s);
const crypto = await loadCrypto(readFileSync(WASM), (buf) => webcrypto.getRandomValues(buf));

test("握手标签、会话密钥、方向密钥与 OpenSSL 一致", () => {
  const k = unhex(V.key), cid = unhex(V.cid);
  assert.equal(hex(crypto.helloTag(k, cid)), V.hello_tag);
  assert.equal(hex(crypto.sessionFromKey(k, cid)), V.session_from_key);
  assert.equal(hex(crypto.sessionFromPake(new Uint8Array(32).fill(0x55), cid)), V.session_from_pake);
  const keys = crypto.channelKeys(unhex(V.session_from_key));
  assert.equal(hex(keys.c2s), V.c2s);
  assert.equal(hex(keys.s2c), V.s2c);
});

test("确认标签与 OpenSSL 一致，校验区分角色", () => {
  const pk = new Uint8Array(32).fill(0x55), cid = unhex(V.cid);
  assert.equal(hex(crypto.confirmTag(pk, "server", cid)), V.confirm_server);
  assert.equal(hex(crypto.confirmTag(pk, "client", cid)), V.confirm_client);
  assert.ok(crypto.verifyConfirm(pk, "client", cid, unhex(V.confirm_client)));
  assert.ok(!crypto.verifyConfirm(pk, "server", cid, unhex(V.confirm_client)));
});

test("加密结果与 OpenSSL 逐字节一致（含 64 位计数器）", () => {
  const k = unhex(V.key);
  assert.equal(hex(crypto.seal(k, 1n, enc("lanshare/2 req /api/rpc"), enc("hello LanShare"))), V.sealed_rpc);
  assert.equal(
    hex(crypto.seal(k, 0x0102030405060708n, enc("lanshare/2 resp /api/down/x/0"), enc("局域网快传"))),
    V.sealed_big_ctr,
  );
});

test("解密：正确的能解开，任何篡改都解不开", () => {
  const k = unhex(V.key), aad = enc("lanshare/2 req /api/rpc");
  const sealed = unhex(V.sealed_rpc);
  assert.equal(new TextDecoder().decode(crypto.open(k, 1n, aad, sealed)), "hello LanShare");
  assert.equal(crypto.open(k, 2n, aad, sealed), null, "错计数器");
  assert.equal(crypto.open(k, 1n, enc("lanshare/2 req /api/up/x/0"), sealed), null, "错接口");
  const bad = sealed.slice(); bad[3] ^= 1;
  assert.equal(crypto.open(k, 1n, aad, bad), null, "篡改");
  assert.equal(crypto.open(k, 1n, aad, sealed.slice(0, 10)), null, "截断");
});

test("大块数据往返（1 MiB + 3）", () => {
  const k = crypto.randomBytes(32);
  const plain = crypto.randomBytes((1 << 20) + 3);
  const sealed = crypto.seal(k, 7n, enc("aad"), plain);
  assert.equal(sealed.length, plain.length + 16);
  assert.deepEqual(crypto.open(k, 7n, enc("aad"), sealed), plain);
});

test("PAKE：同口令同钥匙，错口令不同钥匙，句柄只能用一次", () => {
  const a = crypto.pakeStart("123456", "srv"), b = crypto.pakeStart("123456", "srv");
  assert.equal(a.msg.length, 33);
  const ka = crypto.pakeFinish(a.handle, b.msg), kb = crypto.pakeFinish(b.handle, a.msg);
  assert.ok(ka && kb);
  assert.equal(hex(ka), hex(kb));
  assert.equal(crypto.pakeFinish(a.handle, b.msg), null, "句柄已用过");
  const c = crypto.pakeStart("123456", "srv"), d = crypto.pakeStart("000000", "srv");
  assert.notEqual(hex(crypto.pakeFinish(c.handle, d.msg)), hex(crypto.pakeFinish(d.handle, c.msg)));
});

test("内存不泄漏：反复加解密后线性内存不再增长", () => {
  const k = crypto.randomBytes(32), plain = crypto.randomBytes(1 << 20);
  for (let i = 0; i < 20; i++) crypto.open(k, 1n, enc("a"), crypto.seal(k, 1n, enc("a"), plain));
  const before = crypto.memoryBytes();
  for (let i = 0; i < 200; i++) crypto.open(k, 1n, enc("a"), crypto.seal(k, 1n, enc("a"), plain));
  assert.equal(crypto.memoryBytes(), before);
});

test("测速（记入 docs/bench.md）", () => {
  const k = crypto.randomBytes(32), plain = crypto.randomBytes(1 << 20), aad = enc("lanshare/2 req /api/up/x/0");
  const N = 256;
  let t = performance.now(), sealed;
  for (let i = 0; i < N; i++) sealed = crypto.seal(k, BigInt(i + 1), aad, plain);
  const sealMiBs = N / ((performance.now() - t) / 1000);
  t = performance.now();
  for (let i = 0; i < N; i++) crypto.open(k, BigInt(N), aad, sealed);
  const openMiBs = N / ((performance.now() - t) / 1000);
  console.log(`WASM seal ${sealMiBs.toFixed(0)} MiB/s, open ${openMiBs.toFixed(0)} MiB/s (Node ${process.version}, 1 MiB chunks)`);
  assert.ok(sealMiBs > 50 && openMiBs > 50);
});
