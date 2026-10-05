# LanShare 局域网快传 — 设计说明

日期：2026-10-02
状态：已确认（对话中分两段评审通过）

## 1. 目标

在一台电脑上运行一个小程序，让同一局域网内的手机、平板、电脑**只用浏览器**就能和这台电脑互传文件和文字。

成功标准：

- 手机扫电脑屏幕上的二维码即可进入，手机上不安装任何软件
- 手机 → 电脑上传、电脑 → 手机下载都可用，几 GB 的文件不爆内存
- 文字可以互发，并一键复制
- 同 WiFi 下不知道口令的人无法访问
- 提供单个 `.exe`，在没装 Python 的 Windows 电脑上双击即用

### 需求来源区分

| 用户明确提出 | 设计假设（已确认） |
|---|---|
| 电脑 ↔ 手机双向传文件 | 星型：所有设备连运行程序的那台电脑，不做手机直连 |
| 手机不装软件 | 一个共享文件夹作为中转，所有人可见、可下载 |
| 文字互传 | 电脑端也用同一网页操作；启动时自动打开 |
| 访问口令 | 大文件流式读写，下载支持 Range 断点续传 |
| 打包 exe | 同时在线设备只有几台 |

### 不做（YAGNI）

- 网页上删除文件（在电脑资源管理器里删）
- 手机之间直连 / WebRTC
- 文字消息持久化（仅内存，最多 50 条，重启清空）
- HTTPS

## 2. 技术选型

Python 3.9 标准库 `http.server.ThreadingHTTPServer` + 原生 JS 单页 + `qrcode`（仅生成 SVG，不依赖 Pillow）+ PyInstaller `--onefile`。

代码须兼容 Python 3.9：不用 `match`，不用 `X | None` 类型注解写法。

## 3. 架构

```
 手机浏览器 ─┐
 平板浏览器 ─┼──HTTP──► 电脑上的 lan-share（ThreadingHTTPServer）
 电脑浏览器 ─┘                  │
   (127.0.0.1)                  ├─ 共享文件夹（默认 ~/Downloads/LanShare）
                                └─ 文字消息（内存，最多 50 条）
```

### 模块

| 文件 | 职责 | 依赖 |
|---|---|---|
| `lanshare/main.py` | 解析参数、生成口令、选端口、启动服务、打开浏览器、打印提示 | 其余所有模块 |
| `lanshare/netinfo.py` | 找局域网 IPv4；`pick_lan_ips(addrs)` 为纯函数 | 标准库 |
| `lanshare/auth.py` | 口令、会话 Cookie、按 IP 输错锁定；时钟可注入 | 标准库 |
| `lanshare/storage.py` | 文件名清理、重名改名、列文件、流式写入 + 原子落盘 | 标准库 |
| `lanshare/textboard.py` | 内存文字消息板，线程安全，上限 50 条 | 标准库 |
| `lanshare/server.py` | HTTP 路由与处理 | auth / storage / textboard / qrcode |
| `lanshare/web/index.html` | 单页前端，无任何外部资源 | 无 |

## 4. 接口

所有 `/api/*` 和 `/qr.svg` 需要有效会话 Cookie，否则返回 `401`。

| 请求 | 作用 | 成功响应 |
|---|---|---|
| `GET /` | 返回页面。未登录时页面显示口令输入框 | 200 HTML |
| `GET /?k=<口令>` | 口令正确：设 Cookie 并 `303` 跳回 `/`；错误：计入失败次数，返回页面 | 303 / 200 |
| `POST /api/login` | body `{"pin": "123456"}` | 200 `{"ok": true}` + Set-Cookie；错误 403；锁定中 429 |
| `GET /api/info` | 局域网地址、端口、口令、候选地址 | 200 JSON |
| `GET /api/files` | 文件列表 `[{name, size, mtime}]`，按 mtime 倒序 | 200 JSON |
| `PUT /api/files/<文件名>` | 请求体即文件内容（URL 编码文件名） | 201 `{"name": 实际保存名}` |
| `GET /api/files/<文件名>` | 下载，支持单段 `Range` | 200 / 206 |
| `GET /api/text` | 文字消息列表 `[{id, text, time}]` | 200 JSON |
| `POST /api/text` | body `{"text": "..."}`，最长 10000 字符 | 201 |
| `GET /qr.svg` | 入口二维码：`http://<主IP>:<端口>/?k=<口令>` | 200 SVG |

下载头：`Content-Disposition: attachment; filename*=UTF-8''<百分号编码>`，保证中文名正确。

## 5. 页面

1. 顶部：二维码、局域网地址、口令、其他候选地址
2. 上传区：电脑拖拽 / 手机选文件（多选），逐个上传，显示进度条与速度
3. 文件列表：点击下载
4. 文字消息：输入框 + 列表，每条带"复制"按钮

每 2 秒轮询文件与消息；`document.hidden` 时暂停。复制：优先 `navigator.clipboard`，非安全上下文（手机 HTTP 访问）回退到隐藏 textarea + `execCommand('copy')`。

## 6. 安全

- 口令：每次启动 `secrets` 生成 6 位数字
- 会话：`secrets.token_hex(32)`，Cookie `HttpOnly; SameSite=Lax; Path=/`（Lax 而非 Strict：从扫码 App 跳转进来的首次导航 + 303 跳转在部分浏览器下会丢 Strict Cookie）
- 防爆破：同一 IP 连续输错 5 次锁定 60 秒（`/?k=` 与 `/api/login` 共用计数）
- 文件名清理：取最后一段路径名；去掉 `\ / : * ? " < > |` 与控制字符；去掉首尾的空格和点（开头的点也去掉：不产生隐藏文件，也不会和临时文件前缀 `.lanshare-` 撞上）；Windows 保留名（CON、PRN、AUX、NUL、COM1-9、LPT1-9）前加 `_`；空名改为 `unnamed`；按 UTF-8 截断到 200 字节以内且保留扩展名
- 下载路径：`realpath` 后其父目录必须等于共享目录；不允许访问隐藏临时文件
- 明文 HTTP：README 写明局域网内可被抓包

## 7. 出错处理

| 场景 | 处理 |
|---|---|
| 上传中断 / 字节数少于 Content-Length | 写入隐藏临时文件 `.lanshare-<随机>.part`，不完整则删除；列表不显示临时文件 |
| 重名 | `名字 (1).ext`、`名字 (2).ext`……；选名 + 改名在锁内完成 |
| 磁盘满（OSError） | 507，删临时文件 |
| 无 Content-Length | 411 |
| 文件不存在 | 404 |
| Range 不合法 | 416 |
| 下载时客户端断开 | 静默忽略 `ConnectionResetError` / `BrokenPipeError` / `ConnectionAbortedError` |
| 端口被占用 | 从 `--port`（默认 8000）起依次尝试 10 个 |
| 无局域网 IP | 仍启动，使用 127.0.0.1 并警告 |
| 防火墙 / 公用网络 | 启动时打印提示：需允许"专用网络"，公用网络下手机连不上 |
| 控制台中文 | 开头 `sys.stdout.reconfigure(encoding='utf-8')` |

## 8. 命令行

```
lanshare [--port 8000] [--dir 共享目录] [--no-browser]
```

## 9. 测试

- 单元（`unittest`）：storage 文件名与重名、auth 口令/会话/锁定（假时钟）、netinfo IP 筛选、textboard 上限
- 集成：线程内启动服务（随机端口 + 临时目录），`http.client` 覆盖：未登录 401、登录、20MB 随机文件 sha256 一致、Range、中文名、重名、半途断开无残留、文字收发、二维码 SVG
- 页面：浏览器自动化打开本机页面验证上传、列表、文字
- exe 冒烟：运行打包后的 exe，实际上传/下载/取二维码
- 真机验收（用户）：手机同 WiFi 扫码，传照片/视频、下载文件、复制文字

## 10. 交付物

- 源码 + 测试 + `README.md`（使用说明、防火墙说明、安全说明）+ `CLAUDE.md`
- `build.ps1`：一键打包
- `dist/LanShare.exe`
