// 用 Node 内置 crypto（OpenSSL，与 RustCrypto 独立）计算协议已知向量，供 Rust/WASM 测试比对。
import { createCipheriv, hkdfSync, createHmac } from "node:crypto";

const hex = (b) => Buffer.from(b).toString("hex");
const K = Buffer.from([...Array(32).keys()]);                    // 00..1f
const CID = Buffer.from([...Array(16).keys()].map((i) => 0xa0 + i)); // a0..af
const hkdf = (ikm, salt, info) => Buffer.from(hkdfSync("sha256", ikm, salt, info, 32));
const hmac = (key, ...parts) => { const h = createHmac("sha256", key); parts.forEach((p) => h.update(p)); return h.digest(); };
const seal = (key, ctr, aad, plain) => {
  const nonce = Buffer.alloc(12); nonce.writeBigUInt64BE(BigInt(ctr), 4);
  const c = createCipheriv("chacha20-poly1305", key, nonce, { authTagLength: 16 });
  c.setAAD(aad, { plaintextLength: plain.length });
  return Buffer.concat([c.update(plain), c.final(), c.getAuthTag()]);
};

const helloKey = hkdf(K, Buffer.alloc(0), "lanshare/2 hello");
const ss = hkdf(K, CID, "lanshare/2 session");
const pakeKey = Buffer.alloc(32, 0x55);
const out = {
  key: hex(K), cid: hex(CID),
  hello_key: hex(helloKey),
  hello_tag: hex(hmac(helloKey, "hello", CID)),
  session_from_key: hex(ss),
  session_from_pake: hex(hkdf(pakeKey, CID, "lanshare/2 session")),
  c2s: hex(hkdf(ss, Buffer.alloc(0), "lanshare/2 c2s")),
  s2c: hex(hkdf(ss, Buffer.alloc(0), "lanshare/2 s2c")),
  confirm_server: hex(hmac(pakeKey, "server", CID)),
  confirm_client: hex(hmac(pakeKey, "client", CID)),
  sealed_rpc: hex(seal(K, 1, Buffer.from("lanshare/2 req /api/rpc"), Buffer.from("hello LanShare"))),
  sealed_big_ctr: hex(seal(K, 0x0102030405060708n, Buffer.from("lanshare/2 resp /api/down/x/0"), Buffer.from("局域网快传"))),
};
console.log(JSON.stringify(out, null, 2));
