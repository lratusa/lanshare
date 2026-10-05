# 第 8 章　构建与发布：release 配置、LTO、把网页塞进 exe、一条命令出成品

## 这一步要解决什么

1. **一条命令从源码到成品**：编 WASM、跑所有测试、编正式版、再拿正式版 exe 真跑一遍，任何一步失败就停；
2. **exe 又小又快**：v1（Python + PyInstaller）是 9.3 MB，v2 能做到多小？速度会不会因此变慢？
3. **网页、图标都在 exe 里**：用户只拿到一个文件；
4. **安装、更新、卸载都可靠**：正在运行时也能更新，卸载干净，**不误删别的东西**。

本章所有数字都是在本机（i9-12900H）上实测的。

## 概念 1：debug 和 release，差的不是一点点

`cargo build` 默认是 **debug**（开发）构建：编译快，带完整调试信息，**不优化**。`cargo build --release` 是正式构建：编译慢，充分优化。

用 `examples/seal_speed.rs`（加密再解密 256 MiB，每块 1 MiB）实测：

| 构建方式 | 加密速度 |
|---|---|
| debug | **8 MiB/s** |
| release | **约 1200 MiB/s** |

相差 150 倍。所以：
- **绝不拿 debug 构建测性能**；
- 开发时觉得“Rust 怎么这么慢”，多半是忘了加 `--release`。

## 概念 2：release 配置的几个旋钮

在工作区根目录的 `Cargo.toml` 里：

```toml
[profile.release]
opt-level = 3        # 优化级别：0 不优化，3 最快；"s"/"z" 为体积优化
lto = true           # 链接时优化：把所有 crate 合在一起再优化
codegen-units = 1    # 整个 crate 作为一个单元编译：慢一点，但优化得更彻底
strip = true         # 去掉符号信息
```

逐个打开，看 exe 怎么变（每一行都是真实构建出来的）：

| 配置 | exe 大小 | 编译耗时 |
|---|---|---|
| A. cargo 默认 release（不开 LTO，16 个编译单元） | 2,596,864 字节（2.48 MiB） | 20 秒 |
| B. A + `lto = true` | 2,460,672 字节 | 34 秒 |
| C. B + `codegen-units = 1` | 2,337,792 字节 | 34 秒 |
| D. C + `strip = true`（**项目现在用的**） | 2,337,280 字节（2.23 MiB） | 36 秒 |
| E. D + `opt-level = "z"`（为体积优化） | 1,703,424 字节（1.62 MiB） | 26 秒 |

几个观察：

- **`strip` 在 Windows 上只省了 512 字节**：MSVC 工具链本来就把调试符号放在单独的 `.pdb` 文件里，exe 里几乎没有可去的。Linux 上 strip 的效果要大得多。
- **`opt-level = "z"` 能再小 27%**，看起来很诱人。但速度呢？

| `opt-level` | 加密速度 | 解密速度 |
|---|---|---|
| 3 | 1157 ~ 1236 MiB/s | 1008 ~ 1093 MiB/s |
| "z" | 256 ~ 282 MiB/s | 255 ~ 262 MiB/s |

**省 0.6 MB，慢 4 倍多**。这个程序的卖点之一就是快，所以保持 `opt-level = 3`。千兆网线的上限约 119 MiB/s，两种配置其实都跑得满；但电脑在边加密、边收发、边写盘，加密用的 CPU 越少越好。

另外 `panic` 我保留了默认的 `unwind`（展开），没有改成 `abort`：服务端某个请求的处理代码如果 panic，只会结束那一个请求；改成 `abort` 能再小一点，代价是**整个程序直接退出**。

## 概念 3：泛型在哪里编译？一次真实的提速

传输测试（`tests/transfer.rs`，会上传、下载好几 MiB）要跑 **12.3 秒**，原因是测试用的是 debug 构建，加密只有 8 MiB/s。

Rust 有个常见写法：开发构建里，**只给依赖开优化**，自己的代码保持不优化、方便调试：

```toml
[profile.dev.package."*"]     # "*" = 所有依赖（不含工作区自己的 crate）
opt-level = 3
```

改完实测：加密 8 → **12 MiB/s**，几乎没用。为什么？

加密库 `chacha20poly1305` 的核心函数是**泛型**的。Rust 的泛型是“单态化”的：泛型函数在**用到它的地方**，按具体类型生成一份专用代码，并且按**调用方 crate** 的优化级别编译。调用它的是我们自己的 `lanshare-proto`，它没开优化，所以加密代码也没被优化。

再加一条：

```toml
[profile.dev.package.lanshare-proto]
opt-level = 3
```

结果：

| 开发构建配置 | 加密速度 | 传输测试耗时 |
|---|---|---|
| 默认 | 8 MiB/s | 12.3 秒 |
| 依赖开优化 | 12 MiB/s | 7.0 秒 |
| 依赖 + 协议 crate 开优化 | **1025 MiB/s** | **0.31 秒** |

测试快了 40 倍。注意：`opt-level` 只管优化，**整数溢出检查、`debug_assert!` 在开发构建里照样开着**，该抓的 bug 照样能抓到。

> Python 里没有对应的概念：Python 不编译成机器码，`python -O` 也只是去掉 `assert`，不会让代码变快。

## 概念 4：把网页编进 exe，`include_bytes!` 和 `macro_rules!`

v2 的页面、脚本、WASM、图标全部在 exe 里，运行时不读磁盘。`server/assets.rs`：

```rust
macro_rules! asset {
    ($name:ident, $file:literal, $mime:literal) => {
        pub(crate) async fn $name() -> Response {
            let body: &'static [u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/web/", $file));
            let mut response = body.into_response();
            response.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static($mime));
            response
        }
    };
}

asset!(index, "index.html", "text/html; charset=utf-8");
asset!(wasm, "lanshare.wasm", "application/wasm");
```

- `include_bytes!("路径")`：**编译时**把整个文件读进来，变成程序里的一个 `&'static [u8]` 常量。`'static` 表示它和程序活得一样久，不需要分配、也不会被释放；
- `env!("CARGO_MANIFEST_DIR")`：编译时取 crate 所在目录，路径就不依赖“从哪里运行 cargo”；
- `macro_rules!`：**宏**，按模板生成代码。6 个资源，每个都要一个几乎一样的处理函数，写一次模板，每个资源一行。`$name:ident` 是“一个标识符”，`$file:literal` 是“一个字面量”；
- cargo 会记住 `include_bytes!` 用到了哪些文件，文件一改，下次构建自动重新编译。

**风险**：如果 `lanshare.wasm` 没重新编译、没拷过来，exe 里装的就是旧的。所以互通测试里加了一条：从运行中的 exe 下载每个资源，和仓库里的文件逐字节比对哈希。怎么知道这个测试真管用？往 `style.css` 末尾加一行注释、**不重新编译** exe，再跑：`pass 8, fail 1`，测试确实能抓住。

## 概念 5：`build.rs`，编译前先跑的程序

crate 根目录下的 `build.rs` 会在编译这个 crate **之前**先被编译、运行。LanShare 用它把图标和版本信息嵌进 exe：

```rust
fn main() {
    println!("cargo:rerun-if-changed=web/icon.ico");   // 告诉 cargo：只有图标变了才重跑我
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("web/icon.ico"); // 资源编号 1，托盘图标从这里读
        res.set("FileDescription", "LanShare - LAN file transfer (end-to-end encrypted)");
        res.set("ProductName", "LanShare");
        res.set("OriginalFilename", "LanShare.exe");
        if let Err(e) = res.compile() {
            println!("cargo:warning=嵌入图标失败：{e}");
        }
    }
}
```

- `build.rs` 通过往标准输出打印 `cargo:...` 这样的行，和 cargo 交流；
- 它的依赖写在 `[build-dependencies]` 里，和程序本身的依赖分开；
- 嵌进去之后，资源管理器“属性 → 详细信息”里就能看到描述和产品名，托盘图标也从 exe 自己的资源里读（第 7 章）。

## `build.ps1`：一条命令出成品

```
==> build wasm                  cargo build -p lanshare-wasm --release --target wasm32-unknown-unknown
                                 拷贝到 crates/lanshare/web/lanshare.wasm
==> clippy                      cargo clippy --workspace --all-targets -- -D warnings
==> cargo test                  98 个 Rust 测试
==> node tests (debug exe)      22 个 Node 测试（含真实启动 exe 的互通测试）
==> release build               cargo build -p lanshare --release
==> smoke test (release exe)    同一套互通测试，换成刚编出来的正式版 exe 再跑一遍
Done: dist\LanShare.exe (2282 KB, wasm 93.5 KB)
```

（这是写这一章时的输出。第 9 章安全加固之后是 120 个 Rust 测试、23 个 Node 测试，exe 约 2.3 MiB。）

几个讲究：

- **每一步都检查退出码**，失败就停。不能用 `| grep` 过滤输出来判断成功（这个项目在 v1 时期两次因此把失败当成功）；
- **最后拿正式版 exe 真跑一遍**：release 构建和 debug 构建不是同一个程序（优化、LTO 都不一样），测过 debug 不等于测过 release；
- **脚本只用 ASCII**：Windows 自带的 PowerShell 5.1 读没有 BOM 的脚本时按本地代码页解析，中文会乱。而 `install.ps1` 要显示中文，所以它必须存成 **UTF-8 带 BOM**；
- **WASM 构建结果稳定**：在同一台机器、同一套工具链上重新编译，`lanshare.wasm` 和仓库里提交的逐字节相同（`git status` 里没出现它）。换机器、换编译器版本是否仍然相同，这里没有验证。

## 安装与卸载：读代码时发现的一个隐患

v1 的卸载脚本无条件删除任务栏固定：

```powershell
$Pinned = Join-Path $env:APPDATA "...\User Pinned\TaskBar\局域网快传 LanShare.lnk"
Remove-Item $Shortcut, $Pinned -Force -ErrorAction SilentlyContinue
```

如果同一台电脑上既有“所有用户”版（Program Files），又有“当前用户”版（`-CurrentUser`），两份安装的快捷方式**同名**。卸载当前用户版时会把**另一份**的任务栏固定也删掉。我准备测试当前用户版的安装流程时，读代码发现了这个问题：真跑一遍的话，正在用的 v1 的任务栏图标就没了。

修法：删之前先看快捷方式指向哪里，**只删指向本次卸载的 exe 的**：

```powershell
function Remove-OwnShortcut($path) {
    if (-not (Test-Path $path)) { return }
    $target = (New-Object -ComObject WScript.Shell).CreateShortcut($path).TargetPath
    if ($target -eq $Exe) { Remove-Item $path -Force } else { Log "保留 $path（它指向 $target）" }
}
```

其他按设计文档修复的点：

| 问题 | 修法 |
|---|---|
| 正在运行时安装/卸载会失败 | 自动结束从安装目录启动的进程，等它真正退出再覆盖 |
| 杀毒软件扫描时文件被占用 | 复制、删除都重试 10 次 |
| 从“设置 → 应用”卸载时没有窗口，失败了用户不知道 | 失败时弹窗（30 秒自动关闭） |
| 卸载到一半失败，“设置”里的入口却已经没了 | 注册表项**最后**删 |
| 卸载脚本找错安装目录 | 以脚本自己所在的目录为准，并校验目录名必须是 `LanShare`（**后来改了**：第 9 章的审查 M-1 指出，这个脚本可能被拷进同样叫 LanShare 的共享文件夹里运行；现在以注册表登记的安装位置为准，只删安装时放进去的 3 个文件，目录空了才删） |

**怎么安全地测安装脚本？** 安装脚本会改开始菜单、注册表、`%LOCALAPPDATA%`，直接在自己电脑上跑可能弄坏正在用的东西。办法是**把环境变量指到临时目录**：`LOCALAPPDATA`、`APPDATA` 都改掉，脚本里所有路径就都落在沙箱里，只剩注册表 `HKCU\...\Uninstall\LanShare` 这一项是真实的（测完会被卸载删掉）。在沙箱里走完“安装 → 运行 → 运行中更新 → 运行中经注册表的卸载命令卸载”，17 项检查全部通过；测完对比哈希，真实的任务栏固定和开始菜单快捷方式都没动过。

## 真实踩坑

1. **`failed to remove file ...\LanShare.exe`**：测试用的 LanShare 还在运行时 `cargo build`，Windows 不允许覆盖正在运行的 exe。更麻烦的是，我紧接着重启了服务，跑起来的其实是**旧的** exe。要先停掉进程再编译。
2. **只优化依赖没用**：见概念 3，泛型代码按调用方的优化级别编译。
3. **`strip` 几乎没效果**：Windows 的调试符号本来就不在 exe 里。

## 和 Python 对照

| 要做的事 | Python v1 | Rust v2 |
|---|---|---|
| 出成品 | PyInstaller：把解释器、标准库、依赖打成一个包 | 编译器直接生成机器码 |
| exe 大小 | 9,751,312 字节（9.3 MiB） | 2,337,280 字节（2.2 MiB） |
| 启动时 | onefile 模式**每次启动先把所有东西解压到临时目录** | 直接运行 |
| 网页资源 | `--add-data "lanshare\web;lanshare\web"` 打进包，运行时 `open(os.path.join(WEB_DIR, "index.html"))` 从解压目录读 | `include_bytes!` 编译时嵌进程序，运行时零文件读取 |
| 漏打包资源 | 运行到那一行才报 `FileNotFoundError` | 编译时文件不存在直接编译失败 |
| 构建环境 | 干净的 venv + `pip install`，防止把开发机上乱七八糟的包打进去 | `Cargo.lock` 锁死版本，不需要 venv |
| 优化 | 没有（`-O` 只去掉 `assert`） | `opt-level`、LTO、`codegen-units` |
| 图标、版本信息 | `--icon lanshare\web\icon.ico` | `build.rs` + winresource |

v1 的资源查找：

```python
# v1 server.py：运行时拼路径、打开文件
WEB_DIR = os.path.join(os.path.dirname(os.path.abspath(__file__)), "web")
with open(os.path.join(WEB_DIR, "index.html"), "rb") as f:
    ...
```

打包成 exe 后 `__file__` 指向临时解压目录，所以必须用 `--add-data` 把 `web` 文件夹带上，忘了带，打包能成功、运行时才报错。Rust 的 `include_bytes!` 把这一类问题提前到了编译时。

## 小测验

**1. 有人说“我测了一下，Rust 写的加密只有 8 MiB/s，还不如 Python”。你第一个怀疑什么？**

<details><summary>答案</summary>

他测的是 debug 构建（`cargo run` 不加 `--release`）。本项目实测 debug 8 MiB/s，release 约 1200 MiB/s，相差 150 倍。性能测试必须用 release 构建。
</details>

**2. `opt-level = "z"` 让 exe 小了 27%，为什么项目不用？**

<details><summary>答案</summary>

实测加密速度从约 1200 MiB/s 掉到约 270 MiB/s，慢了 4 倍多。这是一个以“快”为卖点的传输工具，为了省 0.6 MB 牺牲这么多速度不划算。体积优化适合对速度不敏感、对大小很敏感的场景（比如嵌入式设备、要通过网络下载的 WASM）。
</details>

**3. 只给依赖开了优化（`[profile.dev.package."*"]`），加密为什么还是很慢？**

<details><summary>答案</summary>

加密函数是泛型的。Rust 的泛型在**使用它的 crate** 里按具体类型实例化，并按那个 crate 的优化级别编译。调用加密库的是 `lanshare-proto`，它没开优化，所以生成的加密代码也没优化。给 `lanshare-proto` 也开上优化，debug 构建下就到了 1025 MiB/s。
</details>

**4. `include_bytes!` 和 Python 里 `open(path).read()` 读网页文件，有什么区别？**

<details><summary>答案</summary>

`include_bytes!` 在**编译时**读文件，内容成为程序的一部分；文件不存在会直接编译失败。`open().read()` 在**运行时**读，文件不在（比如打包时漏了）要等运行到那一行才报错。前者的代价是改了网页必须重新编译 exe。
</details>

**5. 为什么 `build.ps1` 测完 debug 版还要拿 release 版 exe 再跑一遍互通测试？**

<details><summary>答案</summary>

release 和 debug 是用不同的编译选项生成的两个程序（优化级别、LTO 都不同），而且 release 版内嵌的是这次刚编好、刚拷进去的 WASM。只测 debug，等于没测真正发给用户的那个文件。
</details>

**6. 卸载脚本删任务栏固定之前，为什么要先检查快捷方式指向哪里？**

<details><summary>答案</summary>

“所有用户”版和“当前用户”版的快捷方式同名。不检查的话，卸载其中一份会把另一份的任务栏固定也删掉。只删指向本次卸载的 exe 的快捷方式，才不会误伤。
</details>
