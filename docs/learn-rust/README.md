# 跟着 LanShare 学 Rust

这是一份“边做边学”的教程，**写给有 Python 基础的人**：每一章对应 LanShare v2（Rust 重写版）的一个真实开发阶段。代码不是为教学编的例子，而是项目里正在运行的代码；遇到的编译错误也都是开发中真实碰到的。

## 配套的多媒体课程

讲课用的幻灯片做成了讲义（PDF，可以下载、打印）：[课堂讲义](../../讲义/课堂/)每集对应本书的同号章节，[视频课讲义](../../讲义/视频课/)和视频里的幻灯片一样。讲解在课上：先听课、看讲义建立直觉，再读书看细节，效果最好。

## 怎么用

0. 先读 [第 0 章](00-python-to-rust.md)：用本项目 Python 版（v1）和 Rust 版（v2）的真实代码两两对照，把你熟悉的 Python 概念映射到 Rust。之后每一章也都有一节“和 Python 对照”。
1. 按顺序读。每章开头先说“这一步要解决什么问题”，再讲用到的 Rust 概念，然后对照项目里的真实代码。
2. 每章末尾有小测验，先自己想，再点开答案。
3. 想动手时，照着“动手试试”在本机跑命令（需要已安装 Rust：`rustc --version` 能输出版本即可）。

## 章节

| 章 | 开发阶段 | 学到的 Rust |
|---|---|---|
| [00](00-python-to-rust.md) | （速查） | Python ↔ Rust 概念对照：异常/Result、None/Option、with/作用域、线程/GIL/tokio、len 与 UTF-8 |
| [01](01-cargo-and-workspace.md) | 搭项目骨架、跑通 WASM 探针 | cargo、crate、workspace、edition、编译器报错怎么读 |
| [02](02-proto-ownership-result-traits.md) | 协议与加密核心 `lanshare-proto` | 所有权与借用、`Result` 与 `?`、错误枚举、trait、单元测试 |
| [03](03-wasm-memory-unsafe.md) | 浏览器端 WASM | WebAssembly、线性内存、裸指针与 `unsafe` |
| [04](04-server-async-axum.md) | 服务端（axum） | async/await、tokio、共享状态、serde |
| [05](05-transfer-io-and-wire-test.md) | 分块传输与抓包测试 | 文件 I/O、错误处理、集成测试 |
| [06](06-request-journey.md) | 浏览器 ↔ WASM ↔ 服务端 | 一次加密请求的完整旅程 |
| [07](07-desktop-shell.md) | Windows 桌面外壳（托盘、单实例、无控制台） | `cfg` 条件编译、`unsafe` 调 Windows API、`Drop` 与 `let _` 的坑、消息循环、`Arc<dyn Fn>` 回调 |
| [08](08-build-and-release.md) | 构建、打包、安装 | debug vs release、LTO 与体积/速度取舍、泛型在哪里编译、`include_bytes!` 与宏、`build.rs` |
| [09](09-security-review.md) | 独立安全审查与加固 | 在线猜测的概率、先验请求头再读正文、信号量与超时、`poll` 与 `AsyncRead`、孤儿规则、`os.rename` 与 `fs::rename` 的语义差别、变异测试 |
| [10](10-device-approval.md) | 新设备要在电脑上点“允许” | 想清楚“记住谁”、门放在哪、带数据的枚举与 `matches!`、把时间当参数、锁里只算、`&'static str` |
| [11](11-first-principles-architecture.md) | 回头看整个项目 | 第一性原理：六条推不翻的事实推出主要设计；推论打架时谁让谁；每个决定的代价；以后设计系统的七个步骤 |

（章节随开发进度陆续补全。）
