// LanShare 浏览器端协议客户端。
// 加解密全部在 lanshare.wasm 里完成（与服务端是同一份 Rust 代码）；
// 本文件负责：把字节搬进/搬出 WASM 线性内存、配对、加密请求。

const encoder = new TextEncoder();

/** 把 Uint8Array / ArrayBuffer / URL 加载成加密工具集。 */
export async function loadCrypto(source, getRandomValues = (buf) => globalThis.crypto.getRandomValues(buf)) {
  let memory;
  const imports = {
    env: {
      // WASM 需要随机数时调用；getRandomValues 单次最多 65536 字节，分段取
      ls_random(ptr, len) {
        const target = new Uint8Array(memory.buffer, ptr >>> 0, len >>> 0);
        for (let off = 0; off < target.length; off += 65536) {
          getRandomValues(target.subarray(off, Math.min(off + 65536, target.length)));
        }
      },
    },
  };
  let bytes = source;
  if (!(source instanceof Uint8Array || source instanceof ArrayBuffer)) {
    const response = await fetch(source);
    if (!response.ok) throw new Error("加载加密模块失败（" + response.status + "）");
    bytes = await response.arrayBuffer();
  }
  const { instance } = await WebAssembly.instantiate(bytes, imports);
  const x = instance.exports;
  memory = x.memory;

  // 线性内存增长后旧的 ArrayBuffer 会失效，所以每次都现取视图
  const view = (ptr, len) => new Uint8Array(memory.buffer, ptr, len);

  /** 申请输入/输出缓冲区 → 拷入输入 → 调用 → 拷出输出 → 全部释放。 */
  function call(inputs, outLens, fn) {
    const ins = inputs.map((data) => ({ data, len: data.length, ptr: x.ls_alloc(data.length) >>> 0 }));
    const outs = outLens.map((len) => ({ len, ptr: x.ls_alloc(len) >>> 0 }));
    try {
      for (const b of ins) if (b.len) view(b.ptr, b.len).set(b.data);
      const ret = fn(...ins.map((b) => b.ptr), ...outs.map((b) => b.ptr));
      return { ret, outs: outs.map((b) => view(b.ptr, b.len).slice()) };
    } finally {
      for (const b of [...ins, ...outs]) x.ls_free(b.ptr, b.len);
    }
  }

  const split = (ctr) => {
    const big = BigInt(ctr);
    return [Number((big >> 32n) & 0xffffffffn), Number(big & 0xffffffffn)];
  };
  const bytesOf = (s) => (typeof s === "string" ? encoder.encode(s) : s);

  return {
    seal(key, ctr, aad, plain) {
      const [hi, lo] = split(ctr);
      return call([key, aad, plain], [plain.length + 16], (k, a, p, out) =>
        x.ls_seal(k, hi, lo, a, aad.length, p, plain.length, out)).outs[0];
    },
    open(key, ctr, aad, sealed) {
      if (sealed.length < 16) return null;
      const [hi, lo] = split(ctr);
      const r = call([key, aad, sealed], [sealed.length - 16], (k, a, s, out) =>
        x.ls_open(k, hi, lo, a, aad.length, s, sealed.length, out));
      return r.ret === 0 ? r.outs[0] : null;
    },
    helloTag(k, cid) {
      return call([k, cid], [32], (a, b, out) => x.ls_hello_tag(a, b, out)).outs[0];
    },
    sessionFromKey(k, cid) {
      return call([k, cid], [32], (a, b, out) => x.ls_session_from_key(a, b, out)).outs[0];
    },
    sessionFromPake(pakeKey, cid) {
      return call([pakeKey, cid], [32], (a, b, out) => x.ls_session_from_pake(a, pakeKey.length, b, out)).outs[0];
    },
    channelKeys(sessionSecret) {
      const both = call([sessionSecret], [64], (a, out) => x.ls_channel_keys(a, out)).outs[0];
      return { c2s: both.slice(0, 32), s2c: both.slice(32) };
    },
    confirmTag(pakeKey, role, cid) {
      const r = bytesOf(role);
      return call([pakeKey, r, cid], [32], (a, b, c, out) =>
        x.ls_confirm_tag(a, pakeKey.length, b, r.length, c, out)).outs[0];
    },
    verifyConfirm(pakeKey, role, cid, tag) {
      const r = bytesOf(role);
      return call([pakeKey, r, cid, tag], [], (a, b, c, t) =>
        x.ls_verify_confirm(a, pakeKey.length, b, r.length, c, t, tag.length)).ret === 1;
    },
    pakeStart(pin, serverId) {
      const p = bytesOf(pin), s = bytesOf(serverId);
      const r = call([p, s], [x.ls_pake_msg_len()], (a, b, out) => x.ls_pake_start(a, p.length, b, s.length, out));
      if (r.ret === 0) throw new Error("口令或服务器标识不是有效的 UTF-8");
      return { handle: r.ret, msg: r.outs[0] };
    },
    pakeFinish(handle, peerMsg) {
      const r = call([peerMsg], [32], (a, out) => x.ls_pake_finish(handle, a, peerMsg.length, out));
      return r.ret === 0 ? r.outs[0] : null;
    },
    randomBytes(n) {
      const buf = new Uint8Array(n);
      for (let off = 0; off < n; off += 65536) getRandomValues(buf.subarray(off, Math.min(off + 65536, n)));
      return buf;
    },
    memoryBytes() {
      return memory.buffer.byteLength;
    },
  };
}

// ---------------------------------------------------------------------------
// 协议客户端：配对 + 加密请求 + 分块传输
// ---------------------------------------------------------------------------

const CHUNK = 1 << 20;
const STORE_KEY = "lanshare";

export class Unauthorized extends Error {}
export class WrongPin extends Error {}
export class HttpError extends Error {
  constructor(status) {
    super("HTTP " + status);
    this.status = status;
  }
}
export class ServerError extends Error {
  constructor(code) {
    super(code);
    this.code = code;
  }
}

const hex = (u8) => Array.from(u8, (b) => b.toString(16).padStart(2, "0")).join("");
const unhex = (s) => new Uint8Array(s.match(/../g).map((h) => parseInt(h, 16)));

function b64Encode(u8) {
  let s = "";
  for (const b of u8) s += String.fromCharCode(b);
  return btoa(s);
}

function b64Decode(s) {
  const bin = atob(s);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

const b64urlEncode = (u8) => b64Encode(u8).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");

/** 从 location.hash（“#<base64url>”）取出 32 字节密钥；不是合法密钥返回 null。 */
export function keyFromFragment(hash) {
  const raw = (hash || "").replace(/^#/, "");
  if (!/^[A-Za-z0-9_-]{43}$/.test(raw)) return null;
  try {
    const key = b64Decode(raw.replace(/-/g, "+").replace(/_/g, "/") + "=");
    return key.length === 32 ? key : null;
  } catch {
    return null;
  }
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const utf8 = new TextEncoder();
const fromUtf8 = new TextDecoder();

export class LanShareClient {
  constructor(crypto, { base = "", storage = globalThis.localStorage, fetchImpl } = {}) {
    this.crypto = crypto;
    this.base = base;
    this.storage = storage;
    this.fetch = fetchImpl || ((...a) => globalThis.fetch(...a));
    this.keys = null;
    this.cid = null;
    this.ctr = 0;
    this.serverIdCache = null;
  }

  async serverId() {
    if (!this.serverIdCache) {
      const res = await this.fetch(this.base + "/api/server", { cache: "no-store" });
      if (!res.ok) throw new HttpError(res.status);
      this.serverIdCache = (await res.json()).server_id;
    }
    return this.serverIdCache;
  }

  #remember(key) {
    try {
      this.storage?.setItem(STORE_KEY, JSON.stringify({ server_id: this.serverIdCache, key: b64urlEncode(key) }));
    } catch { /* 隐私模式等存不了也不影响本次使用 */ }
  }

  forget() {
    try { this.storage?.removeItem(STORE_KEY); } catch { /* 忽略 */ }
  }

  /** 记住电脑经加密信道发来的 K（`info` 里的 key）。用口令配对的设备，要等电脑上点了“允许”才拿得到。 */
  rememberKey(fragment) {
    const key = keyFromFragment("#" + fragment);
    if (key) this.#remember(key);
  }

  #useSession(cid, sessionSecret) {
    this.cid = cid;
    this.keys = this.crypto.channelKeys(sessionSecret);
    this.ctr = 0;
  }

  get paired() {
    return this.keys !== null;
  }

  /** 扫码配对：证明知道二维码里的密钥 K，派生本会话的密钥。 */
  async pairWithKey(key) {
    await this.serverId();
    const cid = this.crypto.randomBytes(16);
    const res = await this.fetch(this.base + "/api/hello", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ cid: hex(cid), tag: hex(this.crypto.helloTag(key, cid)) }),
    });
    if (res.status === 403) throw new Unauthorized();
    if (!res.ok) throw new HttpError(res.status);
    this.#useSession(cid, this.crypto.sessionFromKey(key, cid));
    this.#remember(key);
  }

  /** 口令配对（SPAKE2）：口令不经过网络；成功后经加密信道拿到 K 并记住。 */
  async pairWithPin(pin) {
    const serverId = await this.serverId();
    const cid = this.crypto.randomBytes(16);
    const { handle, msg } = this.crypto.pakeStart(pin, serverId);
    let res = await this.fetch(this.base + "/api/pake/start", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ cid: hex(cid), msg: b64Encode(msg) }),
    });
    if (!res.ok) throw new HttpError(res.status);
    const reply = await res.json();
    const pakeKey = this.crypto.pakeFinish(handle, b64Decode(reply.msg));
    if (!pakeKey || !this.crypto.verifyConfirm(pakeKey, "server", cid, unhex(reply.confirm))) throw new WrongPin();
    res = await this.fetch(this.base + "/api/pake/finish", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ cid: hex(cid), confirm: hex(this.crypto.confirmTag(pakeKey, "client", cid)) }),
    });
    if (res.status === 403) throw new WrongPin();
    if (!res.ok) throw new HttpError(res.status);
    this.#useSession(cid, this.crypto.sessionFromPake(pakeKey, cid));
    const info = await this.rpc({ op: "info" });
    const key = keyFromFragment("#" + info.key);
    if (key) this.#remember(key);
  }

  /** 用以前存下的密钥重新握手（换新的会话 id）；没存过或不是这次运行的返回 false。 */
  async resume() {
    let saved = null;
    try { saved = JSON.parse(this.storage?.getItem(STORE_KEY) || "null"); } catch { /* 忽略 */ }
    if (!saved || saved.server_id !== (await this.serverId())) return false;
    const key = keyFromFragment("#" + saved.key);
    if (!key) return false;
    try {
      await this.pairWithKey(key);
      return true;
    } catch (e) {
      if (e instanceof Unauthorized) this.forget();
      return false;
    }
  }

  /** 发一个加密请求，返回解密后的字节。 */
  async send(path, plain) {
    if (!this.keys) throw new Unauthorized();
    const ctr = ++this.ctr;
    const body = this.crypto.seal(this.keys.c2s, ctr, utf8.encode("lanshare/2 req " + path), plain);
    const res = await this.fetch(this.base + path, {
      method: "POST",
      cache: "no-store",
      headers: { "X-LS-Sid": hex(this.cid), "X-LS-Ctr": String(ctr), "Content-Type": "application/octet-stream" },
      body,
    });
    if (res.status === 401) {
      this.keys = null;
      throw new Unauthorized();
    }
    if (!res.ok) throw new HttpError(res.status);
    const sealed = new Uint8Array(await res.arrayBuffer());
    const out = this.crypto.open(this.keys.s2c, ctr, utf8.encode("lanshare/2 resp " + path), sealed);
    if (!out) throw new Error("响应解密失败");
    return out;
  }

  async rpc(request) {
    const out = await this.send("/api/rpc", utf8.encode(JSON.stringify(request)));
    return JSON.parse(fromUtf8.decode(out));
  }

  async #rpcOk(request) {
    const r = await this.rpc(request);
    if (!r.ok) throw new ServerError(r.error);
    return r;
  }

  /** 网络抖动重试 3 次；未登录、业务错误不重试。 */
  async #retry(fn) {
    for (let attempt = 0; ; attempt++) {
      try {
        return await fn();
      } catch (e) {
        if (e instanceof Unauthorized || e instanceof ServerError || attempt >= 3) throw e;
        await sleep(400 * (attempt + 1));
      }
    }
  }

  /** 分块并发上传，返回服务端保存的文件名。onProgress(已发送字节数)。 */
  async uploadFile(file, { onProgress = () => {}, parallel = 3 } = {}) {
    const { upload_id: id } = await this.#rpcOk({ op: "upload_begin", name: file.name, size: file.size });
    const total = Math.ceil(file.size / CHUNK);
    let next = 0;
    let sent = 0;
    const worker = async () => {
      while (next < total) {
        const index = next++;
        const end = Math.min(file.size, (index + 1) * CHUNK);
        const piece = new Uint8Array(await file.slice(index * CHUNK, end).arrayBuffer());
        await this.#retry(async () => {
          const r = JSON.parse(fromUtf8.decode(await this.send(`/api/up/${id}/${index}`, piece)));
          if (!r.ok) throw new ServerError(r.error);
        });
        sent += piece.length;
        onProgress(sent);
      }
    };
    try {
      await Promise.all(Array.from({ length: Math.min(parallel, Math.max(total, 1)) }, worker));
      const done = await this.#rpcOk({ op: "upload_finish", upload_id: id });
      onProgress(file.size);
      return done.name;
    } catch (e) {
      this.rpc({ op: "upload_abort", upload_id: id }).catch(() => {});
      throw e;
    }
  }

  /** 分块并发下载，返回 Blob。`entry` 是列表里的一项 {id, name, size, mtime}。 */
  async downloadFile(entry, { onProgress = () => {}, parallel = 3 } = {}) {
    const total = Math.max(1, Math.ceil(entry.size / CHUNK));
    const parts = new Array(total);
    const expect = utf8.encode(JSON.stringify({ size: entry.size, mtime: entry.mtime }));
    let next = 0;
    let received = 0;
    const worker = async () => {
      while (next < total) {
        const index = next++;
        parts[index] = await this.#retry(async () => {
          const out = await this.send(`/api/down/${entry.id}/${index}`, expect);
          if (out[0] === 1) throw new ServerError(JSON.parse(fromUtf8.decode(out.subarray(1))).error);
          return out.subarray(1);
        });
        received += parts[index].length;
        onProgress(received);
      }
    };
    await Promise.all(Array.from({ length: Math.min(parallel, total) }, worker));
    return new Blob(parts, { type: "application/octet-stream" });
  }
}
