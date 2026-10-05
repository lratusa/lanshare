# 性能测量记录

所有数字都是在下面这台开发机上实测的，测量脚本在仓库里，可以复现。手机真机数据由用户实测后补充。

**开发机：** Intel Core i9-12900H、64 GB 内存、Windows 11；Node v24.12.0；Rust 1.96.0（2026-10-02 测）

## v1（Python）vs v2（Rust）

`node tools/bench.mjs --v1 <v1 exe> --v2 dist\LanShare.exe --mib 256 --runs 5`

| 指标 | v1（Python + PyInstaller） | v2（Rust） | 怎么测 |
|---|---|---|---|
| exe 体积 | 9,751,312 字节（9.3 MiB） | 2,372,608 字节（2.3 MiB） | 文件大小（2026-10-03 重新打包：构建时抹掉了本机路径，见 build.ps1） |
| 冷启动到可访问 | 640 ms（627–666） | **44 ms**（41–45） | 启动进程 → info 文件写出 → `GET /` 返回 200；5 次取中位数，先跑 1 次预热不计 |
| 空闲内存 | 35.9 MiB | **12.9 MiB** | 就绪 3 秒后进程树的工作集之和（v1 是“引导进程 + Python 子进程”两个） |
| 传完 256 MiB 后的峰值内存 | 38.0 MiB | 20.8 MiB | 进程树的峰值工作集。两者都不会把整个文件读进内存 |
| 本机回环上传 | 644 MiB/s（明文） | 174 MiB/s（加密） | 256 MiB 随机数据，3 次取中位数，SHA-256 校验一致 |
| 本机回环下载 | 523 MiB/s（明文） | 180 MiB/s（加密） | 同上 |

**回环传输为什么 v2 慢？** 回环（同一台电脑上发给自己）没有网络瓶颈，测的是“程序本身最快能多快”。v1 是明文整文件传输；v2 每 1 MiB 一块，客户端用 WASM 加密、服务端解密，每块一个请求。瓶颈在**客户端**：浏览器端代码（这里用 Node 跑同一份 `proto.js` + WASM）是单线程的，每块要做

- WASM 加密约 2.7 ms（见下面的 WASM 速度）；
- 读文件片段、拷进 / 拷出 WASM 内存、发请求，约 2–3 ms。

合计每块 5–6 ms，也就是 170–180 MiB/s。服务端不是瓶颈：原生解密超过 1 GiB/s。

**实际使用时会不会变慢**，取决于手机的 CPU 和 WiFi 的实际速度，哪个先到上限就是哪个。见下文“手机真机”。

## 加解密速度

| 项目 | 结果 | 怎么测 |
|---|---|---|
| 服务端原生（ChaCha20-Poly1305，1 MiB 分块） | 加密 1135–1148 MiB/s，解密 1030–1093 MiB/s | `cargo run -p lanshare-proto --example seal_speed --release`，3 次 |
| 浏览器端 WASM（Node 里跑） | 加密 367 MiB/s，解密 357 MiB/s | `tools/bench.mjs`；包含 JS ↔ WASM 的数据拷贝 |
| `lanshare.wasm` 体积 | 95,714 字节（93.5 KiB），含 ChaCha20-Poly1305、SPAKE2、HKDF/HMAC | `build.ps1`（同上命令，加上抹掉本机路径的编译参数）；直接运行上面的命令会多几十字节的本机路径 |
| WASM 开 SIMD（未采用） | 约快 12%（360 → 410 MiB/s） | 为兼容 iOS 16.4 以前的 Safari 没有采用，见进度台账 |

## 编译配置对体积和速度的影响

见 [`docs/learn-rust/08-build-and-release.md`](learn-rust/08-build-and-release.md)：`opt-level = "z"` 能让 exe 小 27%，但加密速度降到约 270 MiB/s（慢 4 倍多），所以保持 `opt-level = 3`。

## 手机真机

（待用户实测后补充：手机型号、WiFi、同一个文件分别用 v1 / v2 上传和下载的耗时。）
