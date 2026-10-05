# 第 3 章　把 Rust 送进浏览器：WebAssembly、线性内存与 `unsafe`

## 这一步要解决什么

手机打开 `http://192.168.x.x` 时，浏览器不提供加密 API。我们把第 2 章的 `lanshare-proto` 编译成 **WebAssembly**，让浏览器调用 Rust 写的加密函数。

难点在于：**JavaScript 和 WASM 之间只能传数字**（整数和浮点数），传不了字符串，也传不了字节数组。那一个 1 MiB 的文件块要怎么交给 Rust 去加密？

## 概念 1：线性内存，一块共享的大数组

每个 WASM 模块都有一块**线性内存**（linear memory）：一个巨大的连续字节数组。在 JS 里它是一个 `ArrayBuffer`，在 Rust 里就是普通的内存。

```
          WASM 线性内存（JS 里叫 memory.buffer）
 ┌────────────┬──────────────────────┬─────────────────┐
 │  Rust 栈   │  ← ptr = 1048576     │                 │
 │  和全局量  │  [1 MiB 明文.......] │  [密文.......]  │
 └────────────┴──────────────────────┴─────────────────┘
                    ↑ JS 写进来           ↑ Rust 写进来，JS 读出去
```

所以“传一个字节数组”实际分四步：

1. JS 调 `ls_alloc(len)`，Rust 申请一块内存，返回它的**地址**（一个整数）；
2. JS 往 `memory.buffer` 的这个地址写入数据；
3. JS 调 `ls_seal(地址, 长度, ...)`，Rust 根据地址和长度读到数据，把结果写到另一块内存；
4. JS 读出结果，再调 `ls_free` 释放内存。

## 概念 2：裸指针与 `unsafe`

Rust 收到的“地址”就是**裸指针** `*const u8`。把它变回切片的代码是这样（`crates/lanshare-wasm/src/lib.rs`）：

```rust
/// SAFETY: 调用方保证 `ptr` 指向至少 `len` 字节的有效内存（`len == 0` 时可以是任意值）。
unsafe fn input<'a>(ptr: *const u8, len: usize) -> &'a [u8] {
    if len == 0 {
        &[]
    } else {
        // SAFETY: 由本函数的调用约定保证。
        unsafe { std::slice::from_raw_parts(ptr, len) }
    }
}
```

编译器**无法证明** `ptr` 后面真的有 `len` 个有效字节，这件事只有写代码的人能担保。所以：

- 函数本身标成 `unsafe fn`：意思是“调用我要小心，必须满足文档里写的条件”；
- 每个 `unsafe { }` 块上面写一行 `// SAFETY:`，说明为什么这里成立。这是 Rust 社区的惯例，审查代码时只盯这些地方就行。

`len == 0` 为什么要特殊处理？`from_raw_parts` 要求指针非空且对齐，长度为 0 时 JS 可能传来 0。这种边界情况，正是 `unsafe` 代码最容易出错的地方。

**本项目的原则：** `unsafe` 只出现在 WASM 的 FFI 边界（和以后调用 Windows API 的地方），协议和加密逻辑全部是安全的 Rust。

### 内存是怎么申请和释放的

```rust
#[unsafe(no_mangle)]
pub extern "C" fn ls_alloc(len: usize) -> *mut u8 {
    let mut buf = Vec::<u8>::with_capacity(len.max(1));
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf);      // 不让 Rust 在函数结束时释放它，交给 JS 保管
    ptr
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ls_free(ptr: *mut u8, len: usize) {
    // SAFETY: ptr 来自 Vec::with_capacity(len.max(1))，按同样的容量重建，析构时释放。
    drop(unsafe { Vec::from_raw_parts(ptr, 0, len.max(1)) });
}
```

`std::mem::forget` 告诉 Rust：“这块内存别管了”，所有权从此交给 JS。`ls_free` 再用 `Vec::from_raw_parts` 把所有权“收回来”，然后 `drop` 释放掉。申请和释放必须一一对应，否则就会内存泄漏。所以测试里专门检查了一项：加解密 200 次之后，线性内存不能继续增长。

## 概念 3：随机数从哪来

PAKE 需要随机数，但浏览器里的 WASM **没有操作系统**，拿不到系统随机源。解决办法是：WASM 声明一个**导入函数**，由 JS 提供实现：

```rust
#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "env")]
unsafe extern "C" {
    fn ls_random(ptr: *mut u8, len: usize);
}

getrandom::register_custom_getrandom!(js_random);   // 告诉 getrandom 库：需要随机数时调 js_random
```

JS 那边用 `crypto.getRandomValues` 实现。它是密码学安全的随机源，**在 http 页面里也能用**（只有 `crypto.subtle` 受安全上下文限制）。

## 概念 4：`thread_local!` 保存 PAKE 的中间状态

PAKE 分两步（start → finish），中间状态得存在 Rust 这边。JS 只拿到一个“句柄”数字：

```rust
thread_local! {
    static PAKES: RefCell<Vec<Option<proto::PakeState>>> = const { RefCell::new(Vec::new()) };
}
```

- `thread_local!`：每个线程一份的全局变量。WASM 是单线程的，所以这就相当于全局表；
- `RefCell`：允许在运行时可变借用（编译期无法证明没有冲突时使用）；
- `Option` + `.take()`：finish 时把状态**拿走**，留下 `None`。同一个句柄再用第二次，就会拿到 `None` 而报错。这样“状态只能用一次”就由类型系统保证了。

## JS 这边的两个坑

**坑 1：内存增长后，旧视图失效。** WASM 申请大块内存时，线性内存可能会扩容，扩容后原来的 `ArrayBuffer` 会被“分离”（detached），之前创建的 `Uint8Array` 视图读出来全是空的。所以 `proto.js` 每次都**现取视图**：

```js
const view = (ptr, len) => new Uint8Array(memory.buffer, ptr, len);
```

**坑 2：64 位整数。** 计数器是 `u64`，老版本的手机浏览器不支持在 JS 和 WASM 之间直接传 64 位整数。所以拆成高低两个 32 位来传：

```js
const split = (ctr) => { const big = BigInt(ctr); return [Number((big >> 32n) & 0xffffffffn), Number(big & 0xffffffffn)]; };
```

Rust 那边再拼回来：`(u64::from(hi) << 32) | u64::from(lo)`。

## 为什么不用 wasm-bindgen

wasm-bindgen 是 Rust↔JS 最常用的工具，能自动生成胶水代码。我们没用它，有三个原因：

1. 它要求额外安装 `wasm-bindgen-cli`，而且版本必须和库严格一致，构建链条更长；
2. 我们只需要导出十几个函数，手写胶水总共只有一百多行；
3. 生成的 `.wasm` 只有 **95.7 KB**，没有多余的东西。

## 验证：三份独立实现，结果完全一致

`web-tests/crypto.test.mjs` 用第 2 章那份 **OpenSSL 算出的已知向量**，来检查**真实编译出的 `lanshare.wasm`**：

| 实现 | 运行在 | 检验方式 |
|---|---|---|
| OpenSSL（Node 内置） | 开发机 | 生成期望值 |
| RustCrypto（原生） | 电脑 | `cargo test -p lanshare-proto` |
| RustCrypto（WASM） | 浏览器 / Node | `node --test web-tests/crypto.test.mjs` |

8 项测试全部通过。测速结果：WASM 加密 **382 MiB/s**，解密 **388 MiB/s**（i9-12900H，Node 24），远高于 WiFi 的实际速度。

## 动手试试

```bash
cargo build -p lanshare-wasm --target wasm32-unknown-unknown --release
node --test web-tests/crypto.test.mjs
```

试试把 `proto.js` 里 `call()` 的 `finally` 块（释放内存的那段）注释掉，再跑测试，看“内存不泄漏”那一项怎么失败。

## 和 Python 对照

**Python 怎么调 C？`ctypes`。** 本章 JS 调 WASM 的方式，和 Python 用 `ctypes` 调 C 动态库几乎一模一样：

```python
import ctypes
lib = ctypes.CDLL("crypto.dll")
buf = ctypes.create_string_buffer(1024)       # 申请一块 C 能读写的内存
lib.seal(key, ctr_hi, ctr_lo, buf, len(buf))  # 只能传指针和数字
result = buf.raw                              # 把结果拷回 Python
```

```js
const ptr = x.ls_alloc(len);                 // 申请一块 WASM 能读写的内存
view(ptr, len).set(data);                    // 把数据写进去
x.ls_seal(k, hi, lo, a, aadLen, ptr, len, out); // 只能传指针和数字
const result = view(out, len + 16).slice();  // 把结果拷出来
x.ls_free(ptr, len);
```

道理一样：跨语言边界只能传**数字**，大块数据靠“共享的一块内存 + 地址”来传。

**为什么不用 Python 做浏览器端？** 浏览器里跑 Python 要用 Pyodide，光解释器就有大约 10 MB。本项目的 Rust WASM 只有 95.7 KB，手机打开页面几乎感觉不到加载时间。

**`unsafe` 在 Python 里有对应吗？** 没有直接对应。`ctypes` 写错指针，Python 进程会直接崩溃（甚至悄悄写坏内存），Python 不会提醒你。Rust 至少把这种危险代码圈在 `unsafe { }` 里，审查时一眼就能找到。

## 小测验

**1. 为什么不能直接把 JS 的 `Uint8Array` 传给 WASM 函数？**

<details><summary>答案</summary>

WASM 函数的参数只能是数字。字节数据要先写进 WASM 的线性内存，再把**地址和长度**这两个数字传进去。
</details>

**2. `// SAFETY:` 注释是写给谁看的？**

<details><summary>答案</summary>

写给审查代码的人（包括将来的自己）。它说明为什么这段 `unsafe` 代码是正确的、依赖调用方保证什么。编译器不读这些注释。
</details>

**3. 为什么 `ls_alloc` 里要调用 `std::mem::forget`？去掉会怎样？**

<details><summary>答案</summary>

不调用的话，`buf` 在函数返回时就被 drop 释放了，交给 JS 的指针立刻指向已经释放的内存（悬垂指针）。JS 往里写数据会破坏 Rust 的堆，属于未定义行为。`forget` 把所有权交给 JS，直到 `ls_free` 再收回。
</details>

**4. PAKE 句柄为什么用 `Option` + `take()`，而不是直接存 `PakeState`？**

<details><summary>答案</summary>

`take()` 把状态拿走并留下 `None`。同一个句柄第二次调用 finish 会拿到 `None` 而返回错误。这样“PAKE 状态只能用一次”就由代码结构保证了，而不是靠程序员记住不要重复使用。
</details>

**5. 线性内存扩容后，之前创建的 `Uint8Array` 会怎样？`proto.js` 是怎么应对的？**

<details><summary>答案</summary>

扩容后原来的 `ArrayBuffer` 会被分离（detached），旧视图的长度变成 0，读不到数据。`proto.js` 不缓存视图，每次读写都基于当前的 `memory.buffer` 现取。
</details>
