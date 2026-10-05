# 第 5 章　分块传输与抓包测试：文件 I/O、错误处理、用测试证明安全

## 这一步要解决什么

1. **大文件**：1.5 GB 的视频不能一次性读进内存，也不能因为 WiFi 断一下就要从头重传。
2. **证明加密真的有效**：“我们加密了”只是一句话。要用测试来**证明**：同一 WiFi 下抓包的人确实什么都看不到。

## 分块：把大文件切成 1 MiB 一块

```
上传：upload_begin(名字, 大小) → 建隐藏临时文件 .lanshare-<id>.part 并预留空间
      → 并发发送第 0、1、2…块（每块单独加密，可以重发）
      → upload_status 查已收到哪些块（断点续传）
      → upload_finish 收齐后改名落盘（重名自动加 (1)）
下载：逐块请求 /api/down/<文件id>/<块号>，浏览器解密后拼起来
```

> **第 9 章安全加固后加的规矩**：每个会话最多 4 个进行中的上传、全局 64 个（“查名额 + 占位”在同一次加锁里完成）；10 分钟没有新块的上传自动作废、删掉临时文件（只查进度不算有动静）；预留空间后磁盘至少还要剩 512 MiB；改名用**不覆盖**的方式（Rust 的 `fs::rename` 在 Windows 上会覆盖同名文件，和 Python 的 `os.rename` 不一样）。这些都是审查者用攻击脚本试出来的问题。

### 概念 1：文件按偏移写入

块可以乱序到达，所以每块都直接写到它在文件里的位置（`crates/lanshare/src/server/transfer.rs`）：

```rust
let mut file = OpenOptions::new().write(true).open(&temp)?;
file.seek(SeekFrom::Start(index as u64 * CHUNK as u64))?;
file.write_all(&data)
```

`OpenOptions` 是“构建器”模式：用链式调用描述想要怎样打开文件。`seek` 把读写位置移到第 index 块的开头。

开始上传时，先 `set_len(size)` **预先占好空间**。磁盘不够的话在这一步就会失败，而不是传到 90% 才发现。

### 概念 2：`?` 遇上 `io::Error`，以及错误分类

```rust
fn io_error_code(e: &io::Error) -> &'static str {
    match e.kind() {
        io::ErrorKind::StorageFull => "disk_full",
        io::ErrorKind::NotFound => "not_found",
        _ => "io_error",
    }
}
```

`io::Error` 有一个 `kind()`，可以区分“磁盘满了”“找不到”等情况。我们把它翻译成简短的错误码，放进**加密的**响应里交给浏览器，由浏览器显示成中文提示。

### 概念 3：所有权在 async 和线程之间的流动

文件 I/O 会阻塞，所以放进 `spawn_blocking`。注意数据是怎么“交出去”的：

```rust
let written = tokio::task::spawn_blocking(move || -> io::Result<()> {
    let mut file = OpenOptions::new().write(true).open(&temp)?;
    file.seek(SeekFrom::Start(index as u64 * CHUNK as u64))?;
    file.write_all(&data)
})
.await;
```

`move ||` 把 `temp`（路径）和 `data`（1 MiB 数据）的**所有权移进**闭包，闭包在另一个线程上运行。编译器会检查：移进去之后，外面就不能再用了。这就保证了不会有两个线程同时访问同一块数据，而且是在**编译期**保证的。

`spawn_blocking(...).await` 的结果是 `Result<io::Result<()>, JoinError>`，有两层：外层表示“那个线程有没有崩溃”，内层表示“写文件有没有成功”。所以 match 里写的是 `Ok(Ok(()))`、`Ok(Err(e))`、`Err(_)` 三种情况。

### 概念 4：上传只属于发起它的设备

```rust
let Some(upload) = uploads.0.get(upload_id).filter(|u| u.owner == cid) else {
    return reply_error("not_found");
};
```

`Option::filter`：存在**并且**满足条件才保留。别的设备拿着你的 upload_id 来写，得到的回答是“不存在”，而不是“没有权限”。不要把“这个 id 是存在的”这种信息透露给别人。

## 抓包测试：用代码模拟“同一 WiFi 下的偷看者”

`crates/lanshare/tests/wire.rs` 在客户端和服务端之间夹了一个 **TCP 中转代理**，把双向的每一个字节都录下来：

```rust
async fn pump(mut from: OwnedReadHalf, mut to: OwnedWriteHalf, tape: Arc<Mutex<Vec<u8>>>) {
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        match from.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                tape.lock().unwrap().extend_from_slice(&buf[..n]);   // 录下来
                if to.write_all(&buf[..n]).await.is_err() { break; } // 再转发
            }
        }
    }
}
```

接着通过这个代理完整走一遍：扫码配对、口令配对、上传 3 MiB 随机文件（名字叫“机密合同-2026终版.pdf”）、下载回来、发一条文字“WiFi 密码是 hunter2-局域网快传”、拉取包含口令和二维码的信息。

然后在录音里**逐个搜索秘密**：

| 搜索的内容 | 结果 |
|---|---|
| 二维码密钥 K（原始字节 / hex / base64url） | 找不到 |
| 口令 PIN | 找不到 |
| 会话秘密、方向密钥 | 找不到 |
| 文件名、文字内容 | 找不到 |
| 文件开头、中间、结尾各 64 字节 | 找不到 |
| 二维码 SVG 的 `<svg` 标签 | 找不到 |

**反向自检**（证明这个测试不是空跑）：
- 录音总长度 > 文件大小的 2 倍：确实录到了上传和下载；
- 本来就是明文的东西**能**找到：请求行 `/api/hello`、会话 id。说明搜索方法本身是有效的。

> 这就是宣传文章里“防窃听”这三个字的证据。

## 真实踩坑：看起来并发，其实是串行

为了测试“多个块并发、乱序到达”，我最初写了一个“不依赖 futures 库的 join_all”：先把所有请求的 future 收集起来，再逐个 `.await`。写完之后才意识到：**Rust 的 future 是惰性的**，没有被 poll 就什么都不做。reqwest 的请求要等到被 `.await` 时才真正发出，所以逐个 await 实际上就是串行。改成 `tokio::spawn`：每个请求变成一个独立任务，立刻开始执行。

> 这和 JavaScript 不一样：JS 的 Promise 一创建就开始执行，Rust 的 Future 不 poll 就不动。

## 动手试试

```bash
cargo test -p lanshare --test wire -- --nocapture     # 抓包测试
cargo test -p lanshare --test transfer resume         # 断点续传
```

试着在 `channel.rs` 的 `sealed_response` 里把 `proto::seal(...)` 改成直接返回明文，再跑抓包测试，看看会发生什么。

## 和 Python 对照

**v1 怎么收文件（`legacy-python/lanshare/storage.py`）：**

```python
def save_stream(self, name, stream, length):
    safe = sanitize_filename(name)
    tmp = os.path.join(self.root, TEMP_PREFIX + secrets.token_hex(8) + TEMP_SUFFIX)
    try:
        with open(tmp, "wb") as f:              # with：块结束自动关文件
            remaining = length
            while remaining > 0:
                chunk = stream.read(min(CHUNK_SIZE, remaining))
                if not chunk:
                    raise IncompleteUpload("还差 %d 字节" % remaining)
                f.write(chunk)
                remaining -= len(chunk)
        with self._lock:
            final = self._free_path(safe)
            os.rename(tmp, final)
        return os.path.basename(final)
    except BaseException:
        _remove_quietly(tmp)                   # 出任何错都删掉临时文件
        raise
```

v1 是**一个请求传完整个文件**，中途断了只能从头再来。v2 改成**分块**，每块单独加密、单独请求，可以重发、可以续传。思路的变化比语言的变化大。

**对应的 Rust 写法：**

```rust
let mut file = OpenOptions::new().write(true).open(&temp)?;   // ? 相当于“出错就 raise”
file.seek(SeekFrom::Start(index as u64 * CHUNK as u64))?;
file.write_all(&data)
// file 在这里离开作用域，自动关闭 —— 相当于 with 块结束
```

| Python | Rust |
|---|---|
| `with open(...) as f:` | `let f = File::open(...)?;`，离开作用域自动关 |
| `f.seek(offset)` | `f.seek(SeekFrom::Start(offset))?` |
| `except OSError as e: if e.errno == errno.ENOSPC` | `match e.kind() { ErrorKind::StorageFull => ... }` |
| `threading.Thread(target=...)` 把阻塞操作挪走 | `tokio::task::spawn_blocking(move \|\| ...)` |
| `asyncio.gather(*tasks)` 真正并发 | `tokio::spawn` 每个任务，再逐个 `.await` 结果 |

**`move` 闭包，Python 里没有的东西。** Python 的 lambda 和线程函数可以随意引用外面的变量，GIL 保证不会同时改坏。Rust 要求明确把变量**移进**闭包（`move`），从此外面不能再用，这样编译器就能证明不会有两个线程同时访问。

## 小测验

**1. 为什么上传开始时就 `set_len(size)`？**

<details><summary>答案</summary>

预先占好磁盘空间。空间不够会立刻失败，而不是传到一半才报错。另外后续每块都可以直接写到自己的偏移位置。
</details>

**2. `spawn_blocking(move || ...)` 里的 `move` 做了什么？为什么必须要有它？**

<details><summary>答案</summary>

把闭包用到的变量（路径、数据）的所有权**移进**闭包。闭包要在另一个线程上运行，而且可能比当前函数活得更久，不能只借用外面的变量。移进去之后编译器保证外面不会再使用它们，也就不存在两个线程同时访问。
</details>

**3. 别的设备用你的 upload_id 写数据，为什么返回 `not_found`，而不是“无权限”？**

<details><summary>答案</summary>

“无权限”会告诉攻击者这个 id 是存在的、猜对了。统一回答“不存在”，不泄露任何信息。
</details>

**4. 抓包测试里的“反向自检”是为了防止什么？**

<details><summary>答案</summary>

防止测试“空跑”：比如代理根本没录到流量，或者搜索函数写错了永远返回 false。这些情况下，“找不到秘密”就毫无意义。确认明文部分能被找到，才说明“找不到秘密”是可信的。
</details>

**5. 下面的代码会同时发出 4 个请求吗？**
```rust
let futures: Vec<_> = urls.iter().map(|u| client.get(u).send()).collect();
for f in futures { f.await; }
```

<details><summary>答案</summary>

**不会**，实际是一个接一个地发。Rust 的 future 是惰性的，`send()` 只是创建了一个 future，要等被 poll（`.await`）时才开始执行。想要并发，可以用 `tokio::spawn`，或者 `futures::join_all`（它会同时 poll 所有 future）。
</details>
