# 第 4 章　服务端：async/await、axum 与共享状态

## 这一步要解决什么

服务端要同时服务好几台设备：手机在上传视频，电脑窗口每 2 秒轮询一次，另一部手机正在输口令。每个请求都要做这几件事：
1. 校验 Host（防 DNS rebinding）；
2. 找到会话，检查计数器（防重放），解密；
3. 处理业务（列文件、加文字……）；
4. 把结果加密后返回。

相关代码在 `crates/lanshare/src/server/` 目录下。

## 概念 1：async/await 和 tokio

如果每个连接都占一个操作系统线程，连接多了就很浪费：线程大部分时间在**等网络**。`async` 函数在等待时会让出 CPU，少量线程就能同时服务成千上万个连接。

```rust
pub async fn serve(listener: tokio::net::TcpListener, state: Arc<AppState>) -> std::io::Result<()> {
    axum::serve(listener, router(state).into_make_service_with_connect_info::<SocketAddr>()).await
}
```

> **后来改了（第 9 章）**：独立安全审查发现，`axum::serve` 不给 hyper 配计时器，“请求头必须多久内收完”的时限根本不生效，只连不说话的连接能永远挂着。现在的 `serve_with_shutdown` 自己接收连接：先按来源 IP 计数准入（本机单独一池），再交给 hyper 并配上计时器（请求头 10 秒时限）。这一章的 async/await 概念不变。

- `async fn` 返回的是一个 **Future**（还没执行完的计算），只有 `.await` 它时才会推进；
- **tokio** 是负责调度这些 Future 的运行时，相当于 async 世界里的“操作系统”；
- 阻塞操作（读目录、写磁盘）不能直接放进 async 函数，否则会卡住 tokio 的工作线程。所以列文件时用了 `spawn_blocking`，把它交给专门跑阻塞任务的线程池：

```rust
let files = tokio::task::spawn_blocking(move || st2.storage.list()).await.unwrap_or_default();
```

## 概念 2：axum 的路由和“提取器”

```rust
Router::new()
    .route("/", get(assets::index))
    .route("/api/hello", post(auth::hello).layer(DefaultBodyLimit::max(4096)))
    .route("/api/rpc", post(channel::sealed).layer(sealed_limit))
    .route("/api/up/{upload}/{index}", post(channel::sealed).layer(sealed_limit))
    .layer(middleware::from_fn(security_headers))
    .layer(middleware::from_fn(check_host))
    .with_state(state)
```

handler 的参数叫**提取器**（extractor）：在参数列表里写上你需要什么，axum 会从请求里取出来交给你：

```rust
pub(crate) async fn sealed(
    state: St,                                   // 共享状态
    ConnectInfo(peer): ConnectInfo<SocketAddr>,  // 对方的 IP 和端口
    headers: HeaderMap,                          // 请求头
    uri: Uri,                                    // 路径
    body: Bytes,                                 // 请求体（原始字节）
) -> Response
```

`ConnectInfo(peer): ConnectInfo<SocketAddr>` 是一种**模式解构**：直接在参数位置把外面那层包装拆掉，拿到里面的 `peer`。

> **后来改了（第 9 章，审查 I-3）**：提取器很方便，但 `body: Bytes` 有个代价：axum 在调用函数**之前**就把整个请求体读进了内存，那时还没检查会话。任何人不需要密钥，开几百个连接各发 1 MiB 不发完，就能占住几百 MiB。现在 `sealed` 只接收一个 `request: Request`，自己控制顺序：`into_parts()` 拆开 → 只看请求头做预检查 → 拿到名额 → 限量、限时读正文。**提取器替你做了事，也就替你决定了顺序**。

```rust
pub(crate) async fn sealed(state: St, ConnectInfo(peer): ConnectInfo<SocketAddr>, request: Request) -> Response {
    let (parts, body) = request.into_parts();            // 正文还没读
    let (cid, ctr, keys) = match precheck(&st, &parts.headers, &path) { ... };  // 不合格：一个字节都不读
    ...
```

`.layer(...)` 用来加**中间件**，它包在 handler 外面，每个请求都会经过。我们的两个中间件：
- `check_host`：Host 不是 IP 地址或 `localhost` 的，一律返回 421。DNS rebinding 攻击必须借助域名，所以 IP 字面量天然不受影响；
- `security_headers`：给每个响应加上 CSP、`nosniff`、`no-referrer` 等安全响应头。

## 概念 3：多个请求怎么共享状态，`Arc<Mutex<T>>`

所有请求要访问同一张会话表。Rust 不允许“多个地方同时可变地访问同一份数据”，否则就是数据竞争。标准做法有两层：

- **`Arc<T>`**（原子引用计数）：让多个任务共同拥有同一份数据，最后一个持有者离开时释放；
- **`Mutex<T>`**（互斥锁）：同一时刻只允许一个任务修改数据。

```rust
pub struct AppState {
    pin: Mutex<String>,
    pub(crate) sessions: Mutex<auth::Sessions>,
    pub(crate) guard: Mutex<auth::Guard>,
    // ...
}
```

整个 `AppState` 放在 `Arc` 里，每个 handler 拿到的是一个 `Arc` 克隆（只是引用计数 +1，数据本身不复制）。

**规则：拿着 `std::sync::Mutex` 的锁时，不能 `.await`。** `.await` 会让出线程，锁却还没释放，其他任务可能卡死。我们的代码里，所有加锁都放在一个很小的 `{ }` 块里，块结束锁就自动释放（这叫 RAII）。

### 并发重放：为什么要锁两次

防重放的核心代码（`channel.rs`）：

```rust
// 第一次加锁：取钥匙、预检计数器（不登记）。解密在锁外做，不拖慢其他设备。
let keys = {
    let mut sessions = st.sessions.lock().expect("锁不会中毒");
    let Some(session) = sessions.get(&cid) else { return Err(StatusCode::UNAUTHORIZED) };
    if !session.window.check(ctr) { return Err(StatusCode::FORBIDDEN) }
    session.keys.clone()
};
let plain = proto::open(&keys.c2s, ctr, &proto::req_aad(path), body).map_err(|_| StatusCode::FORBIDDEN)?;

// 第二次加锁：再查一次再登记，两个相同的重放请求并发到达时只有一个能通过。
let mut sessions = st.sessions.lock().expect("锁不会中毒");
// ...
if !session.window.check(ctr) { return Err(StatusCode::FORBIDDEN) }
session.window.commit(ctr);
```

为什么不在一次加锁里把检查、解密、登记都做完？解密 1 MiB 数据大约需要 1 毫秒，这期间锁着全局会话表，所有设备的请求都得排队。为什么第二次还要再查一遍？假设攻击者同时发出两个一模一样的重放请求，它们都能通过第一次检查，都能解密成功。如果第二次不再检查，就会被处理两次。

> **现在的代码（第 9 章之后）**：两次加锁的逻辑没变，只是拆到了两个函数里：第一次在 `precheck()`（读正文**之前**），第二次在 `open_body()`（解密成功之后）。中间多了“拿名额、限时读正文”。审查者复验时专门测过：同一份密文并发发 10 次，只有 1 次成功。

## 概念 4：serde，把 JSON 变成结构体

```rust
#[derive(Deserialize)]
pub(crate) struct HelloBody {
    cid: String,
    tag: String,
}
```

`#[derive(Deserialize)]` 让编译器自动生成“从 JSON 解析”的代码。JSON 字段缺失或类型不对，解析就会失败。

### 真实踩坑：缺字段时返回的是 422，不是 400

起初我用 `Option<Json<HelloBody>>` 作提取器，以为 JSON 不合法时会得到 `None`。测试 `malformed_hello_is_400` 失败了：**axum 0.8 对“是 JSON 但缺字段”返回的是 422**，只有 Content-Type 不对时才给 `None`。最后改成直接取原始字节，自己解析：

```rust
fn parse<T: DeserializeOwned>(body: &Bytes) -> Option<T> {
    serde_json::from_slice(body).ok()
}
```

这里的 `T: DeserializeOwned` 是**泛型约束**：任何“能从 JSON 解析出来、并且拥有自己数据”的类型都能用这个函数。

教训：框架的默认行为要用测试锁住，不能凭感觉。

## 概念 5：宏，把重复的代码交给编译器

6 个静态文件的 handler 几乎一模一样，所以用 `macro_rules!` 写了一个宏：

```rust
macro_rules! asset {
    ($name:ident, $file:literal, $mime:literal) => {
        pub(crate) async fn $name() -> Response {
            let body: &'static [u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/web/", $file));
            // ...
        }
    };
}

asset!(index, "index.html", "text/html; charset=utf-8");
asset!(wasm, "lanshare.wasm", "application/wasm");
```

`include_bytes!` 在**编译时**把文件内容嵌进 exe，运行时不需要再读磁盘。最终 `LanShare.exe` 是单个文件，网页也在里面。

## 测试：让一个“守规矩的客户端”和“攻击者”分别上场

`tests/common/mod.rs` 写了一个按协议说话的测试客户端：`pair_qr`（扫码配对）、`pair_pin`（口令配对）、`rpc`（加密请求）。`tests/api.rs` 里的 26 个测试中，大约一半是**攻击用例**：

| 攻击 | 期望 |
|---|---|
| 错误的握手标签，连续 5 次 | 403 → 429 锁定 |
| 重放旧的 hello，想把计数器归零 | 会话不重置，旧请求仍然 403 |
| 被淘汰的会话 id 想“复活” | 409 |
| 篡改、重放、挪到别的接口、计数器对不上 | 403 |
| 伪造请求想“占掉”合法计数器 | 合法请求照样通过 |
| 跨站 `text/plain` 请求 | 415 |
| 错误的 Host（DNS rebinding） | 421 |
| 口令猜错 10 次 | 自动换口令（**后来改了**：第 9 章的审查算了一笔账，换口令不降低被猜中的概率。现在是累计 30 次没完成的尝试后**暂停**口令登录，只能在电脑上重新开启） |
| 从局域网 IP 调用“在资源管理器中显示” | `only_local`，桌面操作没有被调用 |
| 日志里出现密钥、口令、文字内容 | 一律不允许 |

另外又做了一次变异测试：把“解密成功后才登记计数器”改成“解密前就登记”，6 个测试立刻失败。

## 真实踩坑：clippy 的建议也要验证

`cargo clippy` 是 Rust 官方的代码检查工具。这次它提了 4 条：

1. **`result_large_err`**：`Result<Opened, Response>` 的错误类型有 128 字节，每次返回都要复制。改成 `Result<Opened, StatusCode>`（2 字节），由调用方去生成响应。
2. **嵌套的 `if let` 可以合并**：Rust 2024 支持 `if let ... && let ... { }` 这种 **let 链**写法。
3. **建议把 `.map(|s| { 修改 s; s })` 换成 `.inspect(...)`**：照做之后反而编译失败，因为 `inspect` 给的是 `&&mut Session`，不能通过它修改字段。最后改成最直白的三行：

```rust
let session = self.live.get_mut(cid)?;
session.last_used = Instant::now();
Some(session)
```

工具的建议也要用编译器和测试来验证。

## 动手试试

```bash
cargo test -p lanshare --test api                      # 26 个服务端测试
cargo test -p lanshare --test api wrong_host -- --nocapture
cargo clippy -p lanshare --all-targets -- -D warnings  # 把所有警告当错误
```

## 和 Python 对照

同一个“分发请求”的功能，v1 和 v2 的写法：

```python
# v1 server.py：手写 if/elif 路由，异常转状态码
def _get(self, url):
    if url.path == "/":
        return self._index(url.query)
    self._require_login()                 # 不通过就 raise HttpError(401)
    if url.path == "/api/info":
        return self._send_json(200, {...})
    ...
    raise HttpError(404, "not found")
```

```rust
// v2 server/mod.rs：声明式路由，每个 handler 的参数由 axum 自动“提取”
Router::new()
    .route("/", get(assets::index))
    .route("/api/hello", post(auth::hello))
    .route("/api/rpc", post(channel::sealed))
```

| | Python v1 | Rust v2 |
|---|---|---|
| 服务器 | `http.server.ThreadingHTTPServer` | `axum` + `tokio` |
| 一个连接 | 一个操作系统线程 | 一个轻量异步任务 |
| 共享状态 | `self.server.state`，各处自己加 `threading.Lock` | `State<Arc<AppState>>`，内部字段用 `Mutex` 包住 |
| 加锁 | `with self._lock:` | `let mut s = st.sessions.lock()...;` 作用域结束自动解锁 |
| JSON | `json.loads(body)` 得到一个 dict，字段缺不缺运行时才知道 | `#[derive(Deserialize)] struct HelloBody`，缺字段直接解析失败 |
| 错误 → 状态码 | `raise HttpError(403, ...)`，在 `_dispatch` 里统一 `except` | `Result<Opened, StatusCode>`，`?` 一路传上去 |

**最大的不同在“忘了加锁”这件事上。** Python 里多个线程同时改一个 dict，忘了加锁也能跑，只是偶尔出错，很难复现。Rust 里共享数据如果不放进 `Mutex`（或其它同步类型），**编译器直接拒绝**。

## 小测验

**1. 为什么列目录要放进 `spawn_blocking`？**

<details><summary>答案</summary>

读目录是阻塞的系统调用。如果直接在 async 函数里调用，会占住 tokio 的工作线程，同一线程上的其他请求都得等。`spawn_blocking` 把它交给专门处理阻塞任务的线程池。
</details>

**2. `Arc<Mutex<T>>` 里，`Arc` 和 `Mutex` 各负责什么？**

<details><summary>答案</summary>

`Arc` 解决“多个任务共同拥有同一份数据”（引用计数，最后一个持有者负责释放）；`Mutex` 解决“同一时刻只允许一个任务修改”（互斥）。
</details>

**3. 为什么持有 `std::sync::Mutex` 的锁时不能 `.await`？**

<details><summary>答案</summary>

`.await` 会让出线程，但锁没有释放。其他任务再去拿这把锁就会阻塞，严重时整个运行时都会卡住。应该在一个小的作用域块里用完锁，块结束时锁自动释放。
</details>

**4. 防重放为什么要“检查 → 解密 → 再检查 + 登记”？只检查一次会有什么问题？**

<details><summary>答案</summary>

解密放在锁外，避免长时间占住全局锁。但这样一来，两个一模一样的重放请求可以同时通过第一次检查。第二次加锁时再检查一遍，并在同一次加锁里登记，保证只有一个能通过。
</details>

**5. Host 校验为什么只放行 IP 地址和 `localhost`？**

<details><summary>答案</summary>

DNS rebinding 攻击要先让受害者的浏览器访问攻击者的**域名**，再把这个域名解析到局域网 IP。浏览器发出的 Host 头就是这个域名。只接受 IP 字面量和 localhost，这类请求就会被直接拒绝（421）。
</details>

**6. `include_bytes!` 和运行时读取文件相比，有什么好处和代价？**

<details><summary>答案</summary>

好处：资源嵌在 exe 里，单文件分发，运行时不依赖磁盘上的文件，也不会被人替换。代价：改了网页要重新编译，exe 体积也会相应变大。
</details>
