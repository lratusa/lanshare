# 第 2 章　协议核心：所有权、`Result`、trait 与测试

## 这一步要解决什么

`lanshare-proto` 是整个项目的心脏，服务端和浏览器都用它：

- **加密信道**：`seal`（加密）/ `open`（解密），ChaCha20-Poly1305
- **配对握手**：二维码密钥的握手标签、会话密钥派生（HKDF / HMAC）
- **口令配对**：SPAKE2
- **防重放**：滑动窗口
- **文件名清理**

这一章借这些代码讲 Rust 最核心的几个概念。

## 概念 1：所有权与借用，`&[u8]` 和 `Vec<u8>` 的区别

看 `seal` 的签名（`crates/lanshare-proto/src/channel.rs`）：

```rust
pub fn seal(key: &[u8; 32], ctr: u64, aad: &[u8], plain: &[u8]) -> Vec<u8>
```

| 写法 | 含义 | 类比 |
|---|---|---|
| `[u8; 32]` | **正好** 32 个字节的数组，长度是类型的一部分 | 一个固定 32 格的盒子 |
| `&[u8; 32]` | 借用一个 32 字节数组（只读，不拿走） | 借来看看那个盒子 |
| `&[u8]` | 借用任意长度的一段字节（切片） | 借来看一段字节，长度运行时才知道 |
| `Vec<u8>` | 自己拥有的、可增长的字节数组 | 自己买的伸缩箱子 |

`seal` **借用**密钥和明文（`&`）：调用方给出去之后还能继续用；函数**返回**一个新的 `Vec<u8>`，所有权交给调用方。这就是 Rust 的**所有权**规则：每块内存有且只有一个主人，主人离开作用域时内存自动释放。没有垃圾回收，也不会忘了释放。

把密钥写成 `&[u8; 32]` 而不是 `&[u8]`：**让编译器保证密钥一定是 32 字节**。传一个 31 字节的切片进来，编译就会失败，而不是运行时才出错。这是 Rust 的一个常见做法：把规则写进类型里。

## 概念 2：`Result` 和 `?`，错误必须被处理

解密可能失败（密钥错、被篡改……）。Rust 没有异常，失败用返回值 `Result` 表达：

```rust
pub fn open(key: &[u8; 32], ctr: u64, aad: &[u8], sealed: &[u8]) -> Result<Vec<u8>, ProtoError> {
    ChaCha20Poly1305::new(Key::from_slice(key))
        .decrypt(&nonce(ctr), Payload { msg: sealed, aad })
        .map_err(|_| ProtoError::Decrypt)
}
```

- `Result<T, E>` 要么是 `Ok(T)`，要么是 `Err(E)`。调用方**必须**处理两种情况，编译器不让你假装它不会失败。
- `.map_err(|_| ProtoError::Decrypt)` 把库的错误换成我们自己的错误类型。`|_| ...` 是闭包（匿名函数），`_` 表示不关心参数。

我们的错误类型是一个**枚举**：

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtoError {
    Decrypt,  // AEAD 认证失败
    Pake,     // PAKE 对端消息格式不对
}
```

注意：`Decrypt` 刻意**不区分**“密钥错”“被篡改”“计数器错”。告诉攻击者具体错在哪里，等于帮他排查。

`?` 运算符是“出错就提前返回”的简写，见 `pake.rs`：

```rust
let key = state.0.finish(peer_msg).map_err(|_| ProtoError::Pake)?;
key.try_into().map_err(|_| ProtoError::Pake)
```

第一行末尾的 `?`：如果是 `Err`，整个函数立刻返回这个错误；如果是 `Ok(v)`，取出 `v` 继续往下走。

## 概念 3：trait，Rust 的“接口”

```rust
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
```

`ChaCha20Poly1305::new(...)` 和 `.encrypt(...)` 这两个方法，分别来自 `KeyInit` 和 `Aead` 这两个 **trait**。trait 规定“一类类型都会做什么”：所有 AEAD 算法都实现 `Aead`，所以换成 AES-GCM，调用代码几乎不用改。

**trait 方法必须先 `use` 进来才能调用。** 漏掉 `use ...Aead`，编译器会提示 “no method named `encrypt` found”，并告诉你应该引入哪个 trait。

我们自己也实现了 trait。比如为错误类型实现 `Display`，让它能被打印：

```rust
impl std::fmt::Display for ProtoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProtoError::Decrypt => write!(f, "解密失败"),
            ProtoError::Pake => write!(f, "口令配对消息无效"),
        }
    }
}
```

`match` 必须覆盖所有分支。以后给 `ProtoError` 加一个新成员，这里不补上就编译不过，不会漏处理。

### 一个安全细节：密钥不能被意外打印

`#[derive(Debug)]` 能自动生成调试输出，但密钥要是被 `{:?}` 打进日志就泄露了。所以 `ChannelKeys` **手写** `Debug`：

```rust
impl std::fmt::Debug for ChannelKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ChannelKeys(<redacted>)")
    }
}
```

## 概念 4：模块与可见性

`lib.rs` 声明子模块，再用 `pub use` 把需要公开的东西挑出来：

```rust
mod channel;     // 对应 src/channel.rs，默认私有
mod handshake;
pub use channel::{open, req_aad, resp_aad, seal};
```

外部用户只能看到 `pub use` 出来的名字，比如 `lanshare_proto::seal`。`channel.rs` 里的 `fn nonce()` 没有 `pub`，外面根本访问不到。好处是内部实现以后怎么改都不会影响别人。

## 测试：已知向量要来自“另一个人”

`tests/` 目录下的每个文件都是一个独立的**集成测试**，只能使用 crate 的公开 API，和真正的用户一样。

关键思路：协议是我们自己定义的，如果“期望值”也由我们的实现算出来，测试就只是自己验证自己。所以我用 **Node.js 内置的 crypto（底层是 OpenSSL）** 按协议独立算出期望值（`web-tests/gen-vectors.mjs`），存成 `tests/vectors.json`：

```rust
fn vectors() -> serde_json::Value {
    serde_json::from_str(include_str!("vectors.json")).unwrap()   // 编译时把文件内容嵌进来
}

#[test]
fn seal_matches_openssl() {
    let v = vectors();
    let k = key(&v);
    assert_eq!(seal(&k, 1, &req_aad("/api/rpc"), b"hello LanShare"), bytes(&v, "sealed_rpc"));
}
```

结果：RustCrypto 和 OpenSSL **逐字节一致**。

### 变异测试：测试真的能抓到错误吗？

测试全绿，不等于测试有用。我故意把 nonce 里计数器的字节序从大端改成小端（`to_be_bytes` → `to_le_bytes`），再跑测试：

```
open_reverses_openssl_ciphertext --- FAILED
seal_matches_openssl --- FAILED
test result: FAILED. 6 passed; 2 failed
```

被抓住了。改回来之后又全部通过。这个技巧叫**变异测试**：故意制造一个 bug，看测试能不能发现它。

## 真实踩坑：编译器拒绝“特洛伊源码”

写文件名测试时，我在注释里直接粘贴了一个真实的“从右到左覆盖”字符（U+202E），结果编译失败：

```
error: unicode codepoint changing visible direction of text present in comment
```

这是 Rust 编译器对 2021 年 **Trojan Source（CVE-2021-42574）** 攻击的防御。双向控制字符能让代码**看起来**是一回事、**实际**是另一回事，审查者肉眼发现不了。我们的文件名清理要防的正是同一招：`a + U+202E + gpj.exe` 在资源管理器里会显示成 `aexe.jpg`。修法是在源码里写转义形式 `\u{202E}`，而不是字符本身。

## 常量时间比较

校验握手标签用的是 `verify_slice`，而不是 `==`：

```rust
pub fn verify_hello(k: &[u8; 32], cid: &[u8; 16], tag: &[u8]) -> bool {
    mac(&hello_key(k), &[b"hello", cid]).verify_slice(tag).is_ok()
}
```

普通的 `==` 比到第一个不同的字节就返回，耗时会泄露“前几个字节对了”。攻击者反复测量响应时间，可以逐字节猜出正确的标签。`verify_slice` 无论哪里不同，耗时都一样。

## 动手试试

```bash
cargo test -p lanshare-proto              # 跑本章全部 27 个测试
cargo test -p lanshare-proto --test pake  # 只跑 tests/pake.rs
node web-tests/gen-vectors.mjs            # 看看 OpenSSL 算出的期望值
```

试着把 `replay.rs` 里的 `if ctr == 0 { return false; }` 删掉，再跑测试，看看哪个测试会失败、为什么。

## 和 Python 对照

**所有权：Python 里谁也不“拥有”数据。** Python 的变量全是引用，数据由垃圾回收器管理：

```python
data = b"secret"
a = data      # a 和 data 指向同一个 bytes 对象
```

Rust 里每块数据有且只有一个主人。`seal(&key, ...)` 里的 `&` 表示“借来看看，不拿走”。函数返回的 `Vec<u8>` 交给调用方，调用方成为新主人。离开作用域就释放，不需要垃圾回收。

**异常 vs `Result`：** 同一件事（解密失败）在两种语言里的写法：

```python
# Python 风格（如果用 cryptography 库）
try:
    plain = aead.decrypt(nonce, sealed, aad)
except InvalidTag:
    return error(403)
```

```rust
// Rust v2（channel.rs）
let plain = proto::open(&keys.c2s, ctr, &proto::req_aad(path), body)
    .map_err(|_| StatusCode::FORBIDDEN)?;
```

Rust 的 `?` 相当于“出错就 return”，但它是写在函数签名里的：返回类型是 `Result`，调用方一看就知道这个函数会失败。Python 的函数签名看不出会抛什么异常。

**常量时间比较，两个版本做法一样：**

```python
# v1 auth.py
if secrets.compare_digest(str(pin).encode("utf-8"), self.pin.encode("utf-8")):
```

```rust
// v2 handshake.rs
mac(&hello_key(k), &[b"hello", cid]).verify_slice(tag).is_ok()
```

**`__repr__` vs `Debug`：** v2 给 `ChannelKeys` 手写了 `Debug`，打印出来是 `ChannelKeys(<redacted>)`。Python 里的等价写法是自己定义 `__repr__`，避免密钥被 `print` 或日志打出来。

**trait vs 鸭子类型：** Python 只要对象“有 `encrypt` 方法”就能用（鸭子类型）；Rust 要求类型**声明**自己实现了 `Aead` trait，并且调用前要 `use` 这个 trait。更啰嗦，但写错了在编译时就能发现。

## 小测验

**1. 为什么 `seal` 的密钥参数写成 `&[u8; 32]`，而不是 `&[u8]`？**

<details><summary>答案</summary>

长度是类型的一部分。编译器保证传进来的一定是 32 字节，传错长度在编译阶段就会失败，不用等到运行时才发现。
</details>

**2. 下面这行代码里的 `?` 做了什么？**
```rust
let key = state.0.finish(peer_msg).map_err(|_| ProtoError::Pake)?;
```

<details><summary>答案</summary>

如果 `finish` 返回 `Err`，先用 `map_err` 换成 `ProtoError::Pake`，然后**立刻从当前函数返回这个错误**。如果返回 `Ok(v)`，就把 `v` 赋给 `key`，继续往下执行。
</details>

**3. 漏写 `use chacha20poly1305::aead::Aead;` 会怎样？**

<details><summary>答案</summary>

编译失败，提示找不到 `encrypt` / `decrypt` 方法，因为它们定义在 `Aead` trait 里。trait 方法必须先把 trait 引入作用域才能调用，编译器的提示通常会直接告诉你缺哪个 `use`。
</details>

**4. 为什么解密失败只返回一个笼统的 `ProtoError::Decrypt`？**

<details><summary>答案</summary>

区分“密钥错”“被篡改”“计数器错”这些细节，等于给攻击者提供调试信息。对外一律只说“解不开”。
</details>

**5. 防重放窗口为什么要把 `check`（检查）和 `commit`（登记）分开？**

<details><summary>答案</summary>

只有**解密成功**之后才能登记计数器。如果检查时就登记，攻击者可以发大量伪造请求（解密必然失败），提前把合法客户端将要用的计数器“占”掉，造成拒绝服务。
</details>

**6. “测试全绿”就说明代码对吗？本章用了什么办法验证测试本身？**

<details><summary>答案</summary>

不一定。本章用了两个办法：① 期望值来自独立实现（OpenSSL），而不是自己的代码；② 变异测试：故意把 nonce 的字节序改错，确认测试会失败。
</details>
