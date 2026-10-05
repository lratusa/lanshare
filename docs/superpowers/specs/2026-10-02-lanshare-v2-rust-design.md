# LanShare v2（Rust 重写 + 端到端加密）设计说明

日期：2026-10-02
状态：方案已由用户确认（“Rust 重写 + 端到端加密”）
前身：v1（Python）见 `docs/superpowers/specs/2026-10-02-lan-share-design.md`，代码保留在 `legacy-python/`

## 1. 目标

保持 v1 的使用方式（电脑运行、手机只用浏览器扫码、不装 App、不上云），把**安全**和**效率**做成真正的卖点：

1. 同一 WiFi 下的**被动窃听者**（抓包）看不到任何文件内容、文件名、文字、口令，也无法冒用会话。
2. 只知道局域网地址、不知道密钥的人，无法访问任何数据；手动口令无法被离线暴力破解。
3. 传输速度不被加密拖慢：加解密吞吐高于常见 WiFi 实际速度。
4. 单个小体积 exe，秒开，无控制台窗口，托盘常驻，单实例，正确响应关机。
5. 修掉 v1 两轮审查遗留的 Important 问题（单实例竞态、安装/卸载、Content-Type/Origin、bidi 文件名等）。

### 诚实边界（文档与宣传都必须遵守）

- 页面本身通过 HTTP 下发。若攻击者在同一网络里**主动**做中间人（ARP 欺骗、恶意热点），可以篡改手机首次加载的页面脚本，从而拿到密钥。这是“手机免装 App、只用浏览器”这类工具的共同上限；自签 HTTPS 也挡不住且会弹吓人警告。宣传口径：**防窃听**、**密钥不经过网络**，不说“绝对安全”“军用级”。
- 流量大小与时间特征（传了多大的文件、何时传）对窃听者仍可见。
- 拿到口令/二维码的人即为可信用户（与 v1 相同的信任模型）。

## 2. 总体架构

```
 手机浏览器 ── HTTP ──┐                 ┌─ 共享文件夹（默认 ~/Downloads/LanShare）
   app.js + proto.js  │                 │
   lanshare.wasm ◄────┤   lanshare.exe  ├─ 文字消息（内存，50 条）
 （Rust→WASM 加解密）  │  axum + tokio   │
 本机应用窗口 ────────┘  （同一份 Rust   └─ 会话表（内存）
   (Edge --app, 127.0.0.1)   加密代码）
 托盘图标（tray-icon，主线程 Win32 消息循环）：打开主界面 / 打开共享文件夹 / 退出
```

Cargo workspace：

| crate | 作用 | 目标平台 |
|---|---|---|
| `crates/lanshare-proto` | 协议常量、HKDF 派生、AEAD 封装/拆封、握手标签、SPAKE2 包装、防重放窗口、文件名清理 | native + wasm32 |
| `crates/lanshare-wasm` | 把 proto 导出成裸 C ABI 给浏览器用（不依赖 wasm-bindgen；随机数由 JS 的 `crypto.getRandomValues` 通过导入函数提供） | wasm32 |
| `crates/lanshare` | 服务端 + 桌面外壳，产出 `LanShare.exe` | x86_64-pc-windows-msvc |

浏览器端文件：`index.html`、`style.css`、`proto.js`（WASM 胶水 + 加密请求 + 配对）、`app.js`（界面）、`lanshare.wasm`、`icon.ico`，全部 `include_bytes!` 进 exe。

## 3. 密钥与配对

服务端每次启动生成：

- `K`：32 字节随机主密钥（二维码密钥）
- `PIN`：6 位数字（手动输入用）
- `server_id`：16 字节随机数（hex）

### 3.1 二维码（主路径）

二维码内容：`http://<局域网IP>:<端口>/#<base64url(K)>`（无填充）。

`#` 之后的片段按浏览器规范**不会出现在任何 HTTP 请求里**，密钥不经过网络（扫码 App、浏览器历史可能记住这个网址，见 SECURITY.md）。页面读取后立即 `history.replaceState` 抹掉地址栏里的片段。

握手：

1. 客户端生成 `cid`（16 字节随机，hex 表示，作为会话 ID）。
2. `POST /api/hello`，JSON `{"cid": hex, "tag": hex(HMAC-SHA256(hello_key, "hello" ‖ cid_bytes))}`，其中 `hello_key = HKDF(K, info="lanshare/2 hello")`。
3. 服务端常量时间校验 tag；通过则建会话（已存在同 cid 时不重置任何状态），`session_secret = HKDF(K, salt=cid_bytes, info="lanshare/2 session")`。返回 `{"ok": true, "server_id": ...}`。
4. 校验失败计入该 IP 的失败次数（5 次/60 秒锁定）。

窃听者能看到 cid 与 tag，但没有 K 无法算出 session_secret；重放 hello 不会创建新会话也不会重置计数器。

### 3.2 手动口令（SPAKE2）

用于不能扫码的场景（电脑对电脑）。SPAKE2 对称模式，群 Ed25519，口令 = PIN 的 ASCII，身份 = `"lanshare/2|" ‖ server_id`。

1. 客户端 `start_symmetric` 得到 `msg_a`；`POST /api/pake/start {"cid", "msg": b64(msg_a)}`。
2. 服务端：先过限流（每 IP 5 次/60 秒，全局 20 次/60 秒）；`start_symmetric` 得到 `msg_b`，`finish(msg_a)` 得 `pake_key`；暂存 `pending[cid] = pake_key`（60 秒过期）；全局失败计数 +1；返回 `{"msg": b64(msg_b), "confirm": hex(HMAC(pake_key, "server" ‖ cid))}`。
3. 客户端 `finish(msg_b)` 得 `pake_key`，校验 server confirm（不一致 = 口令错误，提示用户）。
4. `POST /api/pake/finish {"cid", "confirm": hex(HMAC(pake_key, "client" ‖ cid))}`；服务端校验通过 → 建会话，`session_secret = HKDF(pake_key, salt=cid_bytes, info="lanshare/2 session")`，全局失败计数 −1。
5. 全局失败计数 ≥ 30 时**暂停口令登录**（返回 423），本机主界面显示“重新开启口令登录”；重新开启时换新 PIN、计数清零。（独立安全审查 I-1 后修订：原设计“≥ 10 自动换 PIN”不降低攻击者成功率，只打扰正常用户。）

性质：窃听者拿到全部消息也无法离线猜 PIN；主动攻击者每轮只能试 1 个 PIN，且受限流约束；累计 30 次没完成的尝试后暂停口令登录，只能在本机重新开启（独立审查 I-1 之后的设计，原先的“失败后换 PIN”已去掉，见 docs/SECURITY.md）。

取舍：有人持续发起 PAKE 却不完成，会让口令登录被暂停（手动输入路径被打扰，需要电脑前的人重新开启）；二维码路径不受影响。

### 3.3 会话密钥

```
k_c2s = HKDF(session_secret, info="lanshare/2 c2s")   客户端→服务端
k_s2c = HKDF(session_secret, info="lanshare/2 s2c")   服务端→客户端
```

会话存服务端内存，最多 256 个（超出淘汰最久未用的）；服务端重启即全部失效（新 K）。**被淘汰的 cid 记入“已退役”集合，本次运行内永不再接受**——否则攻击者重放旧 hello 能以同一 cid 重建会话，计数器归零后旧请求就能被重放。hello / PAKE 遇到已退役或已存在于 PAKE 流程中的 cid 一律拒绝，客户端换新 cid 重试。客户端把 `{server_id, K}` 存进 `localStorage`（口令配对的设备经加密信道的 `info` 拿到 K），每次打开页面用 K 以**新 cid** 重新握手，刷新页面无需重扫；不持久化会话计数器，避免刷新、多标签页之间计数器冲突。server_id 不符或握手失败（电脑上的程序重启过）时清除并提示重新扫码/输入口令。（实施中调整，见台账 Task 6 Ruling；代价：K 存在该源的 localStorage 里。）

### 3.4 新设备确认（2026-10-03 补充）

握手成功只说明对方拿到了 K 或口令。局域网里第一次连上来的设备（按 IP 记），它的加密请求在解密成功后要先过“门”：电脑前的人在本机主界面上核对 4 位验证码、点“允许”以后才放行；在此之前 RPC 一律回 `pending_approval`，分块接口 403。本机回环地址不用确认。握手、协议、WASM 都没有改。详见 `2026-10-03-device-approval.md`。

## 4. 加密信道

所有登录后的请求都是 `POST`，请求体与响应体都是 ChaCha20-Poly1305 密文：

- 请求头：`X-LS-Sid: <cid hex>`、`X-LS-Ctr: <u64 十进制>`、`Content-Type: application/octet-stream`
- 请求体：`AEAD(k_c2s, nonce = 0x00000000 ‖ ctr_be64, aad = "lanshare/2 req " ‖ path, plaintext)`
- 响应体：`AEAD(k_s2c, nonce = 0x00000000 ‖ ctr_be64, aad = "lanshare/2 resp " ‖ path, payload)`，HTTP 状态恒为 200（业务错误在加密 JSON 里）
- 计数器：客户端每个请求 +1（所有请求共用），从 1 开始。服务端用**滑动窗口防重放**：记录最高值 `hi` 和窗口内已见集合，拒绝 `ctr ≤ hi − 4096` 或已见过的值；**解密成功后**才登记计数器
- 服务端拒绝时的明文状态码：未知会话 401（客户端重新配对）、解密失败或重放 403、Content-Type 不是 octet-stream 415、超出大小 413
- 每个 (key, nonce) 只用一次：请求计数器唯一 → 请求与响应的 nonce 都唯一（两个方向用不同密钥）

aad 绑定 path，防止把一个接口的密文挪到另一个接口重放。

### 4.1 RPC（`POST /api/rpc`）

明文是 JSON `{"op": ..., ...}`，≤ 64 KiB。

| op | 参数 | 返回 | 备注 |
|---|---|---|---|
| `info` | – | `{server_id, version, urls, pin, qr_svg, local, folder?, chunk}` | `folder` 只给本机；`qr_svg` 内含 K，只经加密信道下发 |
| `list` | – | `{files: [{id, name, size, mtime}]}` | `id` 为本次运行内稳定的随机 16 hex，路径里不出现文件名 |
| `texts` | – | `{texts: [{id, text, time}]}` | 新的在前 |
| `text_add` | `{text}` | `{item}` | 1–10000 字符 |
| `upload_begin` | `{name, size}` | `{upload_id, chunk}` | 检查剩余磁盘空间 |
| `upload_status` | `{upload_id}` | `{received: [index...]}` | 断点续传用 |
| `upload_finish` | `{upload_id}` | `{name}` | 所有块到齐才改名落盘 |
| `upload_abort` | `{upload_id}` | `{}` | 删临时文件 |
| `reveal` | `{id}` | `{}` | 仅本机：`%SystemRoot%\explorer.exe /select,"路径"` |
| `open_folder` | – | `{}` | 仅本机 |

### 4.2 分块上传 `POST /api/up/<upload_id>/<index>`

明文 = 文件第 index 块原始字节（块大小 1 MiB，最后一块可短）。服务端按 `index × CHUNK` 写入隐藏临时文件 `.lanshare-<upload_id>.part`，记录已收块。重复的块覆盖写，幂等。响应明文 `{"ok": true}`。浏览器端同时 3 个块在途。

### 4.3 分块下载 `POST /api/down/<file_id>/<index>`

请求明文为 JSON `{size, mtime}`（来自 `list`，可省略）；响应明文第 1 个字节是状态：`0` 后面是第 index 块原始字节，`1` 后面是 JSON 错误（`not_found` / `changed` / `bad_index` …）。浏览器解密后拼成 `Blob` 触发下载。size/mtime 与请求不符时返回 `changed`。

## 5. 页面与安全响应头

- `GET /`、`/app.js`、`/proto.js`、`/style.css`、`/lanshare.wasm`、`/favicon.ico`：公开静态资源（不含任何秘密）。
- 响应头：`Content-Security-Policy: default-src 'none'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self'; img-src 'self' blob:; connect-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'`、`X-Content-Type-Options: nosniff`、`Referrer-Policy: no-referrer`、`Cache-Control: no-store`。
- **Host 校验**：只接受 IP 字面量和 `localhost`（带端口），其余 421，挡住 DNS rebinding（重绑定必须借助域名）。
- **跨站**：握手接口只收 `application/json`；带外站 `Origin` 的请求 403。
- 页面不使用 `innerHTML` 等 HTML 注入点（测试强制）；二维码 SVG 经 `Blob` + `img` 显示。
- 界面沿用 v1 的设计（电脑双栏、手机单栏、拖放、色标、断线条、深浅色），新增“口令登录”页的 PAKE 流程与“密钥不经过网络”的安全提示。

## 6. 文件与存储

- 文件名清理沿用 v1 规则，并新增：去掉 Unicode 双向控制符（U+202A–202E、U+2066–2069）和其它 `Cf` 类格式字符；Windows 保留名补齐 `CONIN$`、`CONOUT$`、`COM0`、`LPT0`、上标数字版本。
- 共享目录运行中被删自动重建；启动时清理 10 分钟未修改的残留临时文件。
- 重名自动 `名字 (1).ext`；改名在锁内完成（单进程内）。
- 下载只允许列表中出现的普通文件；canonicalize 后必须在共享目录内。

## 7. 桌面外壳（Windows）

- `#![windows_subsystem = "windows"]`：无控制台；`--no-tray` 从终端运行时 `AttachConsole(ATTACH_PARENT_PROCESS)` 输出日志。
- 单实例：命名互斥量 `Local\LanShare-v2`。已存在 → 轮询 `%LOCALAPPDATA%\LanShare\instance.json`（原子写入，含端口与 K）最多 10 秒，打开已有实例的主界面后退出。显式 `--port`/`--dir` 时不参与单实例。
- 主界面：Edge（找不到用 Chrome，再不行用默认浏览器）`--app=http://127.0.0.1:<端口>/#<K>`。
- 托盘：`tray-icon` + `muda`，菜单“打开局域网快传 / 打开共享文件夹 / 退出”，左键单击打开主界面；隐藏窗口必须对 `WM_QUERYENDSESSION` 返回 TRUE（实测验证）。
- 日志：`%LOCALAPPDATA%\LanShare\lanshare.log`（超过 1 MiB 重来），不写入任何密钥、口令、文件内容。
- 致命错误：`MessageBoxW` 弹窗。
- 命令行：`--port`、`--dir`、`--no-browser`、`--no-tray`、`--info-file`（写入 `{port, pin, key, urls}`，供自动化）。
- exe 嵌入图标与版本信息。

## 8. 安装

`install.ps1` / `uninstall.ps1`（UTF-8 BOM）沿用 v1，修复：运行中的实例自动结束（安装=更新、卸载意图明确）；失败用 `WScript.Shell.Popup`（30 秒自动关闭）提示；卸载时注册表项最后删除；卸载按注册表登记的 `InstallLocation` 定位安装目录，只删安装时放进去的文件、目录空了才删（审查 M-1 后修订，原为按 `$PSScriptRoot` 推断）；`explorer.exe` 用绝对路径。

## 9. 测试与验证

| 层 | 内容 |
|---|---|
| proto 单元 | HKDF/AEAD 已知向量与往返、篡改/错 key/错 aad/错 nonce 失败、防重放窗口边界、SPAKE2 正确与错误口令、文件名清理（迁移 v1 用例 + bidi） |
| 服务端集成（Rust） | 随机端口真实启动，用 Rust 测试客户端走 hello/PAKE/rpc/上传/下载/续传；攻击用例：无会话、错 tag、重放、篡改密文、跨接口重放、错 Host、非本机调用 reveal、路径穿越、超大请求 |
| **抓包测试** | 测试里放一个 TCP 转发代理记录全部字节，完整走一遍配对 + 上传 + 下载 + 文字，断言记录里**找不到**文件内容、文件名、文字、K、PIN、session_secret |
| 跨实现互通（Node） | `proto.js` + 真实 `lanshare.wasm` 对真实 exe：配对、上传、下载、文字、续传 |
| 浏览器 | Chrome 远程调试：电脑窗口（127.0.0.1）+ 手机视口（局域网 IP），抓包断言同上 |
| exe 冒烟 | `--no-tray --info-file` 启动 release exe，Node 客户端走完整流程 |
| 桌面 | 真实 exe：托盘、单实例（快速连开两次只有一个）、`WM_QUERYENDSESSION` 返回值、退出清理 |
| 基准 | 与 v1 对比：exe 体积、冷启动时间、空闲内存、回环传输吞吐（含加密）、WASM 加解密 MiB/s；手机真机速度由用户实测 |

## 10. 交付物

- Rust workspace 源码、测试、`build.ps1`、`install.ps1`/`uninstall.ps1`
- `README.md`、`CLAUDE.md`、安全白皮书 `docs/SECURITY.md`（威胁模型 + 诚实边界）
- 学习教程 `docs/learn-rust/`（按开发阶段分章：概念、真实代码讲解、踩坑、截图、小测验），完成后整理成可交互网页
- 公众号文章（安全 + 效率为主，竞品差异经网络核实，数字全部实测）
