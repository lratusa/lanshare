# 第 9 章　请人来攻击：独立安全审查与加固

## 这一步要解决什么

前 8 章写了 98 个 Rust 测试和 22 个 Node 测试，抓包测试也证明了网络上看不到秘密。但这些测试都是**写代码的人**写的：我想到的攻击方式，我都防了；我没想到的，测试里也不会有。

所以这一步请一个“外人”来攻击它：一个没有参与开发的独立审查者（这不等于专业的人工安全审计），只给设计文档和代码，要求：

- **只读**，不许改代码；
- 每个发现都要**亲手验证**：能写攻击脚本（PoC）就写，并把运行结果贴出来；
- 设计文档“诚实边界”里已经声明不防的东西不算漏洞，除非实现比文档说的更弱。

结论：**没有 Critical（严重）**。协议核心（加密、防重放、防会话复活、路径穿越、XSS）经读代码和实测都成立。但有 3 个 Important、4 个 Minor，外加几条零碎的小问题，全部附了 PoC。

| 编号 | 问题 | 审查者的实测 |
|---|---|---|
| I-1 | 口令可以被长期在线猜测 | 从 40 个源地址连发 130 秒，服务端每分钟正好受理 20 次猜测；按这个速度，30 天约 58% 猜中 |
| I-2 | 没传完的上传永不过期 | 同一会话并发 100 个 `upload_begin` 全部成功（上限明明是 64）；之后别的设备再也传不了 |
| I-3 | 不需要密钥就能撑爆内存 | 300 个连接各扣住请求体最后一个字节，服务端内存涨了 526 MiB，60 秒后一个都没断 |
| M-1 | 卸载脚本可能删掉用户文件 | 在叫 `LanShare` 的共享文件夹里运行卸载脚本，整个文件夹连同用户文件被永久删除，还提示“文件没有动” |
| M-2 | 任何网页都能替你发口令尝试 | `text/plain` 类型的跨站请求直接被受理 |
| M-3 | 同名文件可能被悄悄覆盖 | Rust 的 `fs::rename` 在 Windows 上覆盖已存在的文件 |

下面挑几个讲：每个都对应一个 Rust 知识点。

## 概念 1：安全不只是加密，算一算猜口令的概率

SPAKE2 保证了**每次交互只能验证一个口令**，抓包也没法离线穷举。但在线猜呢？

- 限速：全局每分钟 20 次 → 每天 28,800 次；
- 6 位口令，每次猜中的概率是 1/1,000,000；
- 一天至少猜中一次的概率：1 − (1 − 10⁻⁶)^28800 ≈ **2.8%**；30 天 ≈ **58%**。

原来的设计还有一条“失败 10 次自动换口令”。它有用吗？**没有**：攻击者每次猜的都是随机数，口令换不换，每次猜中的概率都是百万分之一，换口令只会打扰正常用户。

修法是给**总量**设上限：累计 30 次没完成的口令尝试后，**暂停**口令登录，只能在电脑上的主界面里点“重新开启”（同时换新口令）。这样每开启一次，被猜中的概率不超过 30/1,000,000。扫码登录完全不受影响。

> 教训：限速只限制了“速度”，没限制“总量”。时间足够长，再慢的猜测也会成功。

## 概念 2：先看请求头，再读请求体

原来的加密接口是这样写的：

```rust
pub(crate) async fn sealed(
    state: St,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,          // ← 问题在这里
) -> Response {
    let opened = match open_request(&st, &headers, &path, &body) { ... };
```

axum 在调用这个函数**之前**，要先把每个参数“提取”出来，`body: Bytes` 的意思就是“把整个请求体读进内存”。所以哪怕请求头里的会话号是瞎编的，服务端也已经把 1 MiB 收进来了。攻击者开 300 个连接、每个都不发最后一个字节，就占住 300 MiB。

修法：参数改成整个 `Request`，自己控制先后顺序：

```rust
pub(crate) async fn sealed(state: St, ConnectInfo(peer): ConnectInfo<SocketAddr>, request: Request) -> Response {
    let st = state.0;
    let (parts, body) = request.into_parts();            // 拆成“头”和“还没读的正文”
    let path = parts.uri.path().to_string();
    let (cid, ctr, keys) = match precheck(&st, &parts.headers, &path) {
        Ok(checked) => checked,                          // 类型、长度、会话、计数器都合格
        Err(status) => return status.into_response(),   // 不合格：正文一个字节都不读
    };
    // 先占本 IP 的名额（满了立刻 503），再排全局的队（最多等 30 秒）
    let Some(_mine) = BodySlot::take(&st, peer.ip()) else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Ok(Ok(_permit)) = tokio::time::timeout(QUEUE_WAIT, st.bodies.clone().acquire_owned()).await else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let body = match tokio::time::timeout(st.body_timeout, axum::body::to_bytes(body, body_limit(&path))).await {
        Ok(Ok(body)) => body,
        Ok(Err(_)) => return StatusCode::PAYLOAD_TOO_LARGE.into_response(),
        Err(_) => return StatusCode::REQUEST_TIMEOUT.into_response(),
    };
```

三个要点：

1. **`into_parts()`**：把请求拆成请求头和正文。正文这时还在网络缓冲区里，没有被读取；
2. **信号量（Semaphore）**：一共 32 张“许可证”，拿到许可证才能读正文，所以同时最多 32 个正文在内存里（每个来源 IP 最多占 8 个，见下文“复验”）。`_permit` 又是第 7 章讲的 RAII：变量离开作用域，许可证自动归还；
3. **`tokio::time::timeout(时长, future)`**：给任何异步操作加时限。超时返回 `Err`，正文迟迟不发完的请求最终拿到 408。

## 概念 3：自己实现 `AsyncRead`，看一眼异步 I/O 的底层

只限制正文还不够：只连上、不说话，或者请求头只发一半的连接，也会永远挂着。hyper（axum 底下的 HTTP 库）其实有“读请求头超时”，但要先给它配一个计时器，而 axum 的 `serve` 没配。

先做的一件事是包一层：自己写一个“连接”类型，里面装着真正的 TCP 连接和一个计时器，**收发任何字节都重新计时，超时就报错**：

```rust
impl AsyncRead for GuardedIo {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        match Pin::new(&mut self.stream).poll_read(cx, buf) {
            Poll::Ready(result) => {
                if result.is_ok() && buf.filled().len() > before {
                    self.touch();                       // 读到了数据：重新计时
                }
                Poll::Ready(result)
            }
            Poll::Pending if self.idle_expired(cx) => Poll::Ready(Err(timed_out())),
            Poll::Pending => Poll::Pending,
        }
    }
}
```

这是第一次看到 Rust 异步的“底层”。`async/await` 背后其实是在反复调用 `poll`：

| 返回值 | 意思 |
|---|---|
| `Poll::Ready(结果)` | 好了，结果在这里 |
| `Poll::Pending` | 还没好。`cx` 里有一个“叫醒器”（Waker），数据到了会通过它通知运行时再来问一次 |

`Pin` 先不用深究：它保证这个值在内存里不会被挪动（有些异步状态里存着指向自己的指针）。读这段代码只要知道“`Pin<&mut Self>` 就是一个特殊的 `&mut self`”就够了。

连接数也要限制。第一版的做法是全局 128 张许可证，拿到才 `accept`。下面“复验”一节会讲到，这个做法本身出了问题。

## 概念 4：孤儿规则，为什么不能给别人的类型实现别人的 trait

第一版连接限制是包在 axum 的 `serve` 外面的。要让 axum 认得出“我的监听器”接进来的连接的对端地址，最直接的写法是给 `SocketAddr` 实现 axum 的 `Connected` trait。真实的编译报错：

```
error[E0117]: only traits defined in the current crate can be implemented for types defined outside of the crate
   --> crates\lanshare\src\server\conn.rs:127:1
    |
127 | impl axum::extract::connect_info::Connected<axum::serve::IncomingStream<'_, GuardedListener>> for SocketAddr {
    |      |                                                                                            |
    |      |                                                                                            `std::net::SocketAddr` is not defined in the current crate
    |      `IncomingStream` is not defined in the current crate
    = note: define and implement a trait or new type instead
```

这叫**孤儿规则**：trait 和类型至少有一个得是你自己 crate 里定义的。为什么？如果允许，两个不同的库都可以给 `SocketAddr` 实现 `Connected`，同时用这两个库时编译器不知道该用哪个。

当时的绕法是：axum 自己已经为“经过 `tap_io` 包装的任何监听器”实现了这个 trait，所以套一层什么也不做的 `tap_io`。后来（见“复验”）干脆不用 axum 的 `serve` 了，自己把对端地址放进请求的扩展字段里：

```rust
let service = hyper::service::service_fn(move |mut request: hyper::Request<hyper::body::Incoming>| {
    // handler 里用 ConnectInfo<SocketAddr> 拿对端地址
    request.extensions_mut().insert(ConnectInfo(peer));
    app.clone().oneshot(request.map(axum::body::Body::new))
});
```

## 概念 5：同一个函数名，不同的语义（Python 用户尤其要小心）

v1 收文件，最后一步是改名：

```python
# v1 storage.py
with self._lock:
    final = self._free_path(safe)
    os.rename(tmp, final)
```

v2 照着写成了：

```rust
if target.exists() {
    continue;
}
match fs::rename(temp, &target) {
    Ok(()) => return Ok(candidate),
    // 别的程序恰好同时建了同名文件：Windows 上 rename 不覆盖，换下一个名字
    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
```

注释说“Windows 上 rename 不覆盖”，这句话对 Python 是对的，对 Rust 是错的。在本机实测：

```
Python  os.rename(a, b)，b 已存在   →  os.rename refused: FileExistsError 183
Python  os.replace(a, b)            →  覆盖
Rust    fs::rename(a, b)，b 已存在   →  Ok(())，b 被覆盖
```

Python 的 `os.rename` 在 Windows 上拒绝覆盖，要覆盖得用 `os.replace`；Rust 的 `fs::rename` 在所有平台上都覆盖。**函数名一样，语义不一样**。把代码从一种语言搬到另一种语言时，这种差别最容易漏掉。

另外，“先查 `exists()` 再改名”本身就有漏洞：查完和改名之间，别的程序可能正好建了同名文件（这叫 TOCTOU，检查时刻和使用时刻不一致）。修法是不查了，直接用**不覆盖**的方式改名，失败了就换下一个名字：

```rust
// 不先查 exists()：“先查再改”之间别的程序可能正好建了同名文件。直接尝试不覆盖的改名，被占了就换下一个
match rename_no_replace(temp, &self.root.join(&candidate)) {
    Ok(()) => return Ok(candidate),
    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
```

`rename_no_replace` 在 Windows 上调用 `MoveFileExW`，标志位传 0（不带“替换已存在文件”）。

## 复验：修复也会带来新问题

修完之后，请同一位审查者用原来的 PoC 对新代码再跑一遍。原来的 7 个问题全部关闭，但它又指出了 5 个**新**问题，其中最重要的一个是我的修复造成的：

> 连接上限是全局的 128 个，不分来源。局域网里一台机器开 128 个连接，每个隔一会儿发一个字节（空闲计时就不会到期），所有人都连不上了，包括电脑自己的主界面，也就包括“重新开启口令登录”那个按钮。修复前，要把服务搞挂得发 GB 级的数据；修复后，门槛反而更低了。

这是安全工作里很常见的情况：**防御措施本身也是新的攻击面**。最终的做法：

1. **不用 axum 自带的 `serve`**，自己接收连接，交给 hyper 时配上计时器，这样 hyper 自带的“请求头必须在 10 秒内收完”就生效了：

```rust
let mut builder = hyper::server::conn::http1::Builder::new();
builder
    .timer(TokioTimer::new())
    .header_read_timeout(state.header_timeout)
    .max_buf_size(64 * 1024); // 本协议的请求头都很小；正文是流式读取的，不受这个缓冲限制
let connection = graceful.watch(builder.serve_connection(io, service));
```

2. **按来源 IP 分开计数**，本机回环单独一个池子，超过上限的连接**立刻关掉**而不是排队：

```rust
pub(crate) fn admit(&self, ip: IpAddr) -> Option<Ticket> {
    let local = ip.is_loopback();
    let mut counts = self.counts.lock().expect("锁不会中毒");
    let mine = counts.per_ip.get(&ip).copied().unwrap_or(0);
    let pool = if local { counts.local } else { counts.lan };
    if mine >= self.per_ip || pool >= if local { self.max_local } else { self.max_lan } {
        return None;
    }
    ...
    Some(Ticket { counts: self.counts.clone(), ip, local })
}
```

`Ticket`（票）实现了 `Drop`：连接关闭、票被丢弃时，名额自动退还。这是这个项目里第三次用到“离开作用域自动清理”：第 7 章的单实例锁、本章的正文许可证，还有这张票。读正文的名额也同样按 IP 分开（每个 IP 最多 8 个）。

3. **口令暂停的并发漏洞**：原来是先做 SPAKE2 计算、再给“未完成尝试”计数加一。几十个请求同时到达时，它们都通过了“是否已暂停”的检查，上限 30 实际能到 49。改成在同一次加锁里“检查暂停 + 限速 + 先占位”，再去计算。

这个并发漏洞的测试还有个小插曲：第一次写的测试**修复前就通过了**。原因是 `#[tokio::test]` 默认用单线程运行时，20 个请求其实是一个接一个处理的，根本没有并发。改成 `#[tokio::test(flavor = "multi_thread", worker_threads = 8)]` 之后，修复前稳定复现出 8~9 次（上限是 6），修复后正好 6 次。

4. 另外两条：一整块的读取时限从 60 秒放宽到 5 分钟，很慢的网络也能传完（空闲由连接层的 60 秒计时管）；“只查进度不传块”不再让上传保持不过期，检查剩余空间和预占空间改成一个接一个做。

修完又做了第三轮复验，专门看新的连接层：前一轮的 5 个问题全部关闭，没有新的 Important；又找出 3 个 Minor，最主要的是握手接口（hello、PAKE）读正文**没有时限**，一个连接每隔 25 秒发一个字节，100 秒后还挂着。修法和加密接口一样：限量、限时（10 秒）读正文，同样先写测试、再修、再做变异测试。

即便如此，`docs/SECURITY.md` 仍然如实写着：**局域网里的人可以让服务变慢或连不上**（比如用很多个 IP 占满名额）。现在能保证的是不会因此泄密、不会把内存或磁盘耗尽，本机界面始终有自己的名额。

## 用变异测试证明修复有用

每个修复都先写测试（`tests/hardening.rs`，现在 20 个），确认测试在修复前**失败**（第一轮 13 个红、2 个本来就过：那两个是防止“修过头”的护栏，比如“正常登录不计入暂停次数”）。修完全部变绿。

然后做变异测试：把修复故意改回去，对应测试必须失败。

| 故意改坏 | 抓住它的测试 |
|---|---|
| 空闲计时永不到期 | `idle_connections_are_closed` |
| 去掉每会话上传名额 | `uploads_per_session_are_capped` |
| 去掉 Origin 检查 | `cross_site_origin_is_refused` |
| 不检查会话就去读正文 | `unknown_session_is_rejected_before_the_body_arrives` |
| 去掉每 IP 连接上限 / 本机不单独一池 / 去掉请求头时限 / 去掉每 IP 正文名额 | 各自对应的测试 |
| `rename_no_replace` 改回 `fs::rename` | **第一次没抓住** |
| 去掉 `pake_start` 里的“已暂停”检查 | **第一次没抓住** |

两个“活下来”的变异体，原因不一样：

- `rename_no_replace`：`finalize` 先查了 `exists()`，测试里没有“查完之后有人抢先建文件”的情况。`exists()` 把问题**掩盖**了。去掉 `exists()`（本来就该去掉，见概念 5）之后就抓住了。
- “已暂停”检查：完成配对的接口也有同样的检查（两道防线），去掉第一道，第二道照样把整个流程拦住，测试只看了最终结果。给测试加一句“第一步本身就要返回 423”之后就抓住了。**有几道防线，就要分别测几道**。

## 真实踩坑

1. **变异测试“恢复”后，测试还是失败。** 脚本用 `shutil.move` 把备份挪回原处，备份的修改时间比刚才编译时**更早**。cargo 靠修改时间判断源文件有没有变，结论是“没变”，于是没重新编译，跑的还是变异体。改成恢复后更新修改时间（`os.utime`）就好了。
2. **测试期待 401，实际收到 413。** 测试里声明的正文长度照抄了审查者的 PoC（1 MiB + 64 KiB），新的预检查限制更严（1 MiB + 16 字节），还没查会话就因为太长被拒了。两种结果都是“正文没被读进来”，但测试要验证的是“未知会话”这条路，所以把长度改成刚好合法，另加一条断言专门测“太长”。**测试要走到你想测的那条路上**。
3. **新加的限制让旧测试失败**：“并发 20 个请求”的测试开始随机失败，报错是连接被重置。原因是 20 个请求都从 127.0.0.1 来，超过了新加的“每 IP 16 个连接”。这说明限制生效了；那个测试要测的是别的东西，所以在它的配置里把每 IP 上限调大。
4. **一行多余的代码清空了这一章。** 用 Python 脚本改这个 Markdown 文件时，最后多写了一行 `open(P, "w").write(open(P).read())`。Python 先执行 `open(P, "w")`，文件当场被清空，再去读，读到的就是空的，整章没了。这一章当时还没提交，git 也救不回来，只能照着之前写的内容重写一遍。两条教训：写文件之前别先截断它（要改就“读进来 → 改 → 写回去”分三步）；写完一章就提交。
5. **性能“回归”的排查：先做对照实验，再下结论。** 加固之后复测，下载速度从 180 MiB/s 掉到 160 左右。我第一个怀疑的是新加的 `max_buf_size(64 KiB)`（hyper 的这个设置同时限制读和写的缓冲）。对照实验：去掉这一行重新编译、再测两次，结果还是 155~163，说明不是它。再看机器：同一台电脑上另外两个程序正在忙，上传速度也一起掉了。结论是测量时机器不空闲，不是代码变慢。所以这一行保留（它限制了内存），最终的基准要在机器空闲时重测。**怀疑某处改动导致变慢，就只改那一处对比；两次结果一样，就去找别的原因**。

## 和 Python 对照

| 要做的事 | Python | Rust |
|---|---|---|
| 限制同时进行的任务数 | `sem = asyncio.Semaphore(32)`，`async with sem:` | `Semaphore::new(32)`，`let _permit = sem.acquire_owned().await` |
| 给异步操作加时限 | `await asyncio.wait_for(coro, 60)`，超时抛 `TimeoutError` | `tokio::time::timeout(Duration::from_secs(60), fut).await`，超时返回 `Err` |
| 改名不覆盖 | Windows 上 `os.rename` 本来就不覆盖 | 要自己调 `MoveFileExW`（`fs::rename` 会覆盖） |
| 改名并覆盖 | `os.replace` | `fs::rename` |
| 给别人的类加方法 | 随时可以（猴子补丁），v1 就这样给 pystray 打过补丁 | 孤儿规则禁止；用 newtype 或库提供的扩展点 |
| “检查 + 占位”不被并发打断 | `with lock:` 里一起做 | `Mutex` 的同一个 guard 作用域里一起做（上传名额、口令暂停都是这样修的） |
| 资源用完自动归还 | `with` 块结束 | 实现 `Drop` 的值离开作用域（许可证、连接的“票”） |

Python 的 `async with sem:` 和 Rust 的 `let _permit = ...` 是同一个思路：进入时拿许可证，离开作用域时归还。区别是 Python 靠 `with` 语法，Rust 靠变量的作用域。

## 小测验

**1. 限速是每分钟 20 次，为什么还说“30 天约 58% 被猜中”？“失败 10 次换口令”为什么没用？**

<details><summary>答案</summary>

每分钟 20 次，一天就是 28,800 次，一天至少猜中一次的概率约 1 − (1 − 10⁻⁶)^28800 ≈ 2.8%，30 天约 58%。换口令没用，是因为攻击者每次猜的都是随机数：不管口令换没换，每次猜中的概率都是百万分之一。真正有效的是限制**总量**：累计 30 次失败就暂停，必须有人在电脑前重新开启。
</details>

**2. handler 的参数写成 `body: Bytes`，有什么安全隐患？**

<details><summary>答案</summary>

axum 在调用 handler 之前就要把所有参数提取出来，`Bytes` 意味着先把整个请求体读进内存。这时还没检查会话，所以任何人发来的请求都会先占用内存。改成接收 `Request`，用 `into_parts()` 拆开，先检查请求头，合格了再读正文。
</details>

**3. 下面两行有什么区别？哪一行会让“同时最多 32 个正文”的限制失效？**
```rust
let _ = sem.clone().acquire_owned().await;
let _permit = sem.clone().acquire_owned().await;
```

<details><summary>答案</summary>

第一行：`let _ =` 让许可证当场被丢弃，立刻归还，限制失效。第二行：`_permit` 一直持有到作用域结束。这是第 7 章 `let _` 陷阱的又一次出现。
</details>

**4. `poll_read` 返回 `Poll::Pending` 时，运行时怎么知道什么时候再来问？**

<details><summary>答案</summary>

通过 `cx`（Context）里的 Waker。底层的 TCP 连接在返回 `Pending` 前登记了这个 Waker，数据到了就“叫醒”运行时，运行时再调用一次 `poll_read`。`GuardedIo` 里的计时器也登记了同一个 Waker，所以超时的时候也会被叫醒。
</details>

**5. 为什么不能直接 `impl Connected<...> for SocketAddr`？**

<details><summary>答案</summary>

孤儿规则：`Connected` 是 axum 定义的 trait，`SocketAddr` 是标准库的类型，都不属于我们的 crate。如果允许，两个不同的库可能给同一个类型实现同一个 trait，编译器就不知道该用哪个。解决办法是用自己定义的类型（newtype），或者用库已经提供的扩展点（这里先是 `tap_io`，后来干脆自己接收连接）。
</details>

**6. 把 Python 的 `os.rename(tmp, final)` 翻译成 Rust 的 `fs::rename(tmp, &final)`，在 Windows 上行为一样吗？**

<details><summary>答案</summary>

不一样。目标已存在时，Python 的 `os.rename` 在 Windows 上抛 `FileExistsError`（不覆盖），Rust 的 `fs::rename` 直接覆盖。Python 里要覆盖得用 `os.replace`。移植代码时，同名函数的语义可能不同，要查文档或实测。
</details>

**7. 第一版连接上限是“全局 128 个”，为什么复验说它让攻击门槛变低了？**

<details><summary>答案</summary>

名额不分来源：一台机器占满 128 个连接，别人（包括电脑自己的主界面）就都进不来了。而占住连接的成本极低：隔一会儿发一个字节，空闲计时就不会到期。修复前要发 GB 级的数据才能把服务搞挂，修复后几百个字节就够了。最终改成按 IP 计数、本机单独一池、请求头有绝对时限。**防御措施本身也要被审查**。
</details>
