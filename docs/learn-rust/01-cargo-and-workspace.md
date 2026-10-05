# 第 1 章　cargo、crate 与 workspace：先把项目搭起来

## 这一步要解决什么

LanShare v1 是 Python 写的。v2 要做三件 Python 做不好的事：

1. **浏览器里也要能加密。** 手机用 `http://192.168.x.x` 打开页面时，浏览器出于安全规定不提供加密 API（WebCrypto 只在 HTTPS 或 localhost 下可用）。我们要自己带一份加密代码进浏览器。Rust 可以编译成 **WebAssembly（WASM）**，在浏览器里以接近原生的速度运行。
2. **服务端和浏览器用同一份加密代码。** 同一个 Rust 函数，既编译进 `LanShare.exe`，又编译进 `lanshare.wasm`。两端行为一致，测试一次两端受益。
3. **小、快、单文件。** Rust 编译出的是原生机器码，没有解释器，没有“先解压再启动”。

> 要诚实地说：Python 本身也是内存安全的语言，“换成 Rust 就更安全”并不成立。v2 的安全提升来自**协议设计**（端到端加密），Rust 的作用是让这套设计能在浏览器里高效落地。

## 概念：Rust 项目的三层结构

```
workspace（工作区）   ← 一个仓库，统一管理多个 crate，共用一个 target/ 编译目录和 Cargo.lock
 ├─ crate（包）        ← 编译的基本单位：一个库（lib）或一个程序（bin）
 │   └─ module（模块） ← crate 内部按文件/mod 划分的命名空间
 ├─ crate
 └─ crate
```

- **cargo** 是 Rust 的构建工具兼包管理器：`cargo build` 编译，`cargo test` 跑测试，`cargo add 包名` 加依赖，依赖从 crates.io 下载。
- **crate** 有两种：库（`lib.rs` 为入口，给别人用）和可执行程序（`main.rs` 为入口，有 `fn main()`）。

LanShare v2 的 workspace：

| crate | 类型 | 编译到 | 作用 |
|---|---|---|---|
| `lanshare-proto` | 库 | 电脑 + 浏览器 | 协议和加密核心（两端共用） |
| `lanshare-wasm` | 库（cdylib） | 浏览器 | 把 proto 的函数导出给 JavaScript |
| `lanshare` | 程序 | 电脑 | 服务端 + 托盘 + 窗口，产出 `LanShare.exe` |

## 对照真实代码：`Cargo.toml`

仓库根目录的 `Cargo.toml` 声明 workspace：

```toml
[workspace]
resolver = "3"
members = ["crates/lanshare-proto", "crates/lanshare-wasm", "crates/lanshare"]

[workspace.package]
version = "2.0.0"
edition = "2024"

[profile.release]
opt-level = 3        # 最高优化
lto = true           # 链接时跨 crate 优化，体积更小、更快
codegen-units = 1    # 牺牲编译速度换运行速度
strip = true         # 去掉调试符号
```

每个成员 crate 有自己的 `Cargo.toml`，用 `version.workspace = true` 继承公共字段。`lanshare` 依赖 `lanshare-proto` 用的是**路径依赖**：

```toml
[dependencies]
lanshare-proto = { path = "../lanshare-proto" }
```

`lanshare-wasm` 里有一行特殊配置：

```toml
[lib]
crate-type = ["cdylib", "rlib"]
```

`cdylib` 的意思是“编译成 C 风格的动态库”。对 WASM 目标来说，就是产出一个能被 JavaScript 加载的 `.wasm` 文件。

### 什么是 edition

Rust 每三年左右发布一个 **edition**（2015、2018、2021、2024），可以在不破坏旧代码的前提下调整语法规则。每个 crate 在 `Cargo.toml` 里声明自己用哪个 edition，不同 edition 的 crate 可以互相依赖。我们用最新的 **2024**。下面的第一个报错就来自 2024 的新规则。

## 真实踩坑：WASM 探针的 4 个报错

动手写正式代码之前，我先写了一个小“探针”，验证最关键的技术点：SPAKE2 和 ChaCha20-Poly1305 能不能编译成 WASM，在浏览器里跑。

探针要做的事：WASM 里需要随机数，但浏览器里的 WASM 不能直接读操作系统的随机源。所以让 JavaScript 提供一个函数 `ls_random`，WASM 调用它。

### 报错 1–3：`extern blocks must be unsafe`

最初的写法（2021 edition 的习惯）：

```rust
extern "C" { fn ls_random(ptr: *mut u8, len: usize); }

#[no_mangle]
pub extern "C" fn probe_seal(len: usize) -> usize { /* ... */ }
```

编译器报错：

```
error: extern blocks must be unsafe
error: unsafe attribute used without unsafe
error: unsafe attribute used without unsafe
```

**原因：** 2024 edition 要求把“编译器无法替你检查的东西”显式标成 `unsafe`。
- `extern "C" { ... }` 声明的是外部函数，Rust 无法验证它的签名是否和真实实现一致，所以整个块要写成 `unsafe extern "C"`。
- `#[no_mangle]` 让函数名原样导出。如果和别的符号重名，会在链接时出现未定义行为，所以要写成 `#[unsafe(no_mangle)]`。

**修正：**

```rust
unsafe extern "C" { fn ls_random(ptr: *mut u8, len: usize); }

#[unsafe(no_mangle)]
pub extern "C" fn probe_seal(len: usize) -> usize { /* ... */ }
```

> 体会：Rust 的 `unsafe` 不是“危险代码”，而是“我（程序员）担保这里正确，编译器你别管”。2024 edition 让这种担保在代码里更显眼，方便审查。

### 报错 4：`undefined symbol: ls_random`

```
rust-lld: error: ...wprobe...rcgu.o: undefined symbol: ls_random
```

**原因：** 链接器在找 `ls_random` 的实现，但它根本不在 Rust 这边，而是要等 JavaScript 加载 WASM 时传进来。需要告诉链接器：这是一个**从外部导入**的函数，来自名为 `env` 的模块。

**修正：**

```rust
#[cfg(target_arch = "wasm32")]               // 只在编译到 WASM 时生效
#[link(wasm_import_module = "env")]          // 这个函数由宿主（JS）的 env 模块提供
unsafe extern "C" { fn ls_random(ptr: *mut u8, len: usize); }
```

JavaScript 加载时就这样提供它：

```js
const { instance } = await WebAssembly.instantiate(wasmBytes, {
  env: { ls_random: (ptr, len) => crypto.getRandomValues(new Uint8Array(memory.buffer, ptr, len)) },
});
```

`#[cfg(...)]` 是**条件编译**：同一份源码在编译到电脑时跳过这段，编译到 WASM 时才包含。

### 探针结果

| 项目 | 结果 |
|---|---|
| SPAKE2 在 WASM 里运行，两端密钥一致 | ✅ |
| WASM 体积（加密 + PAKE + 密钥派生） | 90 KB |
| WASM 加密速度（Node.js，开发机） | 280–370 MiB/s |

完整探针代码见 [probe-lib.rs.txt](probe-lib.rs.txt)。

## 动手试试

```bash
cargo new hello-rust        # 创建新项目
cd hello-rust
cargo run                   # 编译并运行，输出 Hello, world!
cargo add rand              # 加一个依赖，看看 Cargo.toml 变了什么
```

在 LanShare 仓库根目录：

```bash
cargo build --workspace     # 编译全部 3 个 crate
cargo test --workspace      # 跑全部测试
```

## 和 Python 对照

| 你在 Python 里做的 | 在 Rust 里 |
|---|---|
| `python -m venv .venv` 建虚拟环境 | 不需要。依赖装在全局缓存，每个项目按自己的 `Cargo.lock` 取用，互不干扰 |
| `pip install requests` | `cargo add reqwest` |
| `requirements.txt` | `Cargo.toml` 的 `[dependencies]` |
| `pip freeze > requirements.txt` 锁版本 | `Cargo.lock` 自动生成、自动更新 |
| `python main.py` | `cargo run` |
| PyInstaller 打包 exe（把解释器一起塞进去，启动时先解压） | `cargo build --release`，直接编译成机器码 |
| 一个仓库里放多个包 | workspace |
| Python 3.8 → 3.12 升级可能让旧代码跑不了 | edition 由每个 crate 自己声明，2021 和 2024 的代码可以互相依赖 |

v1 的打包流程（`legacy-python/build.ps1`）要先建一个干净的 venv，装 PyInstaller，再把 Python 解释器、qrcode、Pillow、pystray 一起打进 9.3 MB 的 exe；运行时先解压到临时目录。v2 只需要 `cargo build --release`。

还有一个对照：v1 测试里检查报错信息，靠的是 Python 在**运行时**抛出的异常；本章那 4 个 Rust 报错全部发生在**编译时**，程序还没运行，编译器就指出了问题。

## 小测验

**1. workspace 里的多个 crate 共用什么？（多选）**
A. 同一个 `target/` 编译目录　B. 同一个 `Cargo.lock`　C. 同一个 `main.rs`　D. 同一个 edition（强制）

<details><summary>答案</summary>

**A、B。** 一个 workspace 共用编译目录和锁文件，所以依赖版本一致、重复的依赖只编译一次。每个 crate 有自己的入口文件，edition 也可以各自不同（本项目统一用 2024，是我们自己的选择）。
</details>

**2. `crate-type = ["cdylib"]` 对 WASM 目标意味着什么？**

<details><summary>答案</summary>

产出一个可以被外部宿主（这里是 JavaScript）加载的 `.wasm` 模块，其中 `#[unsafe(no_mangle)] pub extern "C"` 的函数会作为导出项。
</details>

**3. 为什么 2024 edition 要求写 `unsafe extern "C"`？**

<details><summary>答案</summary>

外部函数的签名是程序员声明的，编译器无法验证它和真实实现一致。声明错了（比如参数类型不对）会导致未定义行为。把它标成 `unsafe`，表示由程序员担保正确，审查代码时一眼能找到这些需要格外小心的地方。
</details>

**4. `undefined symbol: ls_random` 为什么不是“忘了写函数”？**

<details><summary>答案</summary>

`ls_random` 本来就不该由 Rust 实现，它由 JavaScript 在加载 WASM 时传入。错误在于没有告诉链接器它是“导入项”。加上 `#[link(wasm_import_module = "env")]` 后，链接器就把它记成一个从 `env` 模块导入的函数。
</details>

**5. 判断：把 LanShare 从 Python 换成 Rust，本身就让它更安全。**

<details><summary>答案</summary>

**错。** Python 也是内存安全的语言。v2 真正的安全提升来自端到端加密协议。Rust 的价值在于：同一份加密代码可以编译到浏览器（WASM）和电脑两端，而且又小又快。
</details>
