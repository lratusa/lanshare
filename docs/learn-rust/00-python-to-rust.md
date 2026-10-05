# 第 0 章　写给会 Python 的人：Rust 对照速查

这个仓库里有**同一个程序的两个版本**：
- `legacy-python/`：v1，Python 写的（你已经在用的那个）
- `crates/`：v2，Rust 重写的

两边很多函数是一一对应的，所以这一章不用课本例子，直接拿**本项目里的真实代码**两两对照。后面每一章也都有一节“和 Python 对照”。

## 一句话记住两者最大的区别

> **Python 在运行时检查，Rust 在编译时检查。**
>
> Python 让你先跑起来，错了再报；Rust 先逼你把各种情况想清楚，编译通过之后很少在运行时出意外。
> 所以写 Rust 时“编译器老是报错”是正常的，它把 Python 里要在运行时才暴露的问题提前了。

## 概念对照表

| Python | Rust | 本项目里的例子 |
|---|---|---|
| `pip install` + `requirements.txt` | `cargo add` + `Cargo.toml` | `crates/lanshare/Cargo.toml` |
| `pip freeze` 锁版本 | `Cargo.lock`（自动生成） | 仓库根的 `Cargo.lock` |
| `venv` 虚拟环境 | 不需要，每个项目的依赖天然隔离 | — |
| PyInstaller 打包（带着解释器） | `cargo build --release` 直接出原生 exe | v1 exe 9.3 MB，v2 见第 8 章 |
| `list` | `Vec<T>` | `Vec<FileEntry>` |
| `dict` | `HashMap<K, V>` | `HashMap<String, String>`（文件 id 表） |
| `set` | `HashSet<T>` / `BTreeSet<T>` | 退役会话表、防重放窗口 |
| `collections.deque` | `VecDeque<T>` | 文字消息板 |
| `bytes` / `bytearray` | `&[u8]`（借来看）/ `Vec<u8>`（自己拥有） | `seal(..., plain: &[u8]) -> Vec<u8>` |
| `str`（按字符） | `String` / `&str`（按 **UTF-8 字节**） | 文件名截断 |
| `None` | `Option<T>`：`None` / `Some(x)` | `name_for(id) -> Option<String>` |
| 抛异常 `raise` | 返回 `Result<T, E>`：`Ok` / `Err` | `open(...) -> Result<Vec<u8>, ProtoError>` |
| `try/except` | `match` / `?` | 见下文 |
| `with open(...)` 自动关闭 | 变量离开作用域时自动 `drop` | 文件、锁都一样 |
| `with lock:` | `let guard = lock.lock()`，作用域结束自动解锁 | 会话表 |
| 类 + 方法 | `struct` + `impl` | `struct Storage` + `impl Storage` |
| 鸭子类型 / `abc.ABC` | `trait` | `trait Desktop` |
| `__repr__` | `impl Debug` | 密钥结构手写 Debug，打印成 `<redacted>` |
| `f"{x}"` | `format!("{x}")` | 日志行 |
| `threading` + GIL | 线程 / `tokio` 异步任务，没有 GIL | 服务端 |
| `asyncio` | `tokio` | 服务端 |
| `unittest` / `pytest` | `#[test]` + `cargo test` | `tests/*.rs` |
| `ctypes` 调 C 函数 | `extern "C"` + `unsafe` | WASM 导出、Windows API |

## 对照 1：异常 vs `Result`（文字消息板）

**Python v1**（`legacy-python/lanshare/textboard.py`）：

```python
def add(self, text):
    """新增一条消息并返回它；空白、超长或不是字符串时抛 ValueError。"""
    if not isinstance(text, str) or not text.strip():
        raise ValueError("文字不能为空")
    if len(text) > MAX_TEXT_LENGTH:
        raise ValueError("文字超过 %d 字" % MAX_TEXT_LENGTH)
    with self._lock:
        item = {"id": self._next_id, "text": text, "time": self._clock()}
        ...
```

**Rust v2**（`crates/lanshare/src/textboard.rs`）：

```rust
pub fn add(&self, text: &str) -> Result<TextItem, &'static str> {
    if text.trim().is_empty() {
        return Err("empty_text");
    }
    if text.chars().count() > MAX_TEXT_CHARS {
        return Err("text_too_long");
    }
    let mut guard = self.inner.lock().expect("文字板的锁不会中毒");
    ...
}
```

逐行看区别：

1. **不需要 `isinstance(text, str)`。** 参数类型写着 `&str`，编译器保证调用方只能传字符串进来。Python 要在运行时自己检查。
2. **出错是“返回”，不是“抛出”。** 返回类型 `Result<TextItem, &'static str>` 写明了：要么成功拿到一条消息，要么拿到一个错误码。调用方**必须处理**这两种情况，不能像 Python 那样忘了写 `try` 而让异常一路冒上去。
3. **陷阱：`len()` 的含义不一样。** Python 的 `len(text)` 数的是**字符**；Rust 的 `text.len()` 数的是 **UTF-8 字节**（一个汉字 3 字节）。所以“最多 10000 字”在 Rust 里要写 `text.chars().count()`。测试 `rejects_blank_and_too_long` 就是专门守这一点的：10000 个汉字必须能发出去。
4. **`with self._lock:` 对应 `let mut guard = self.inner.lock()`。** `guard` 离开作用域时自动解锁，和 `with` 块结束时解锁是一回事。

## 对照 2：`None` vs `Option`

Python 里“找不到”通常返回 `None`，然后**自觉**地检查：

```python
name = self._by_id.get(file_id)   # 可能是 None
if name is None:
    return error("not_found")
use(name)                         # 万一忘了上面的检查，这里就是 None 的 bug
```

Rust 里“可能没有”是一个类型 `Option<String>`，**不拆开就用不了里面的值**：

```rust
let Some(name) = st.files.lock().expect("锁不会中毒").name_for(file_id) else {
    return down_error("not_found");
};
// 到这里 name 一定是 String，不可能是“空”
```

Python 里最常见的 `AttributeError: 'NoneType' object has no attribute ...`，在 Rust 里根本编译不过。

## 对照 3：`dict.get(k, 默认值)` vs `entry().or_default()`（防暴力猜口令）

**Python v1**（`auth.py`）：

```python
count = self._failures.get(ip, 0) + 1
if count >= MAX_FAILURES:
    self._failures.pop(ip, None)
    self._locked_until[ip] = now + LOCK_SECONDS
else:
    self._failures[ip] = count
```

**Rust v2**（`server/auth.rs`）：

```rust
let failures = guard.hello_failures.entry(ip).or_default();   // 没有就插入一个空队列，返回它的可变引用
if recent(failures, now) >= st.per_ip_limit {
    return error(StatusCode::TOO_MANY_REQUESTS, "too_many_attempts");
}
```

`entry(key).or_default()` 相当于 Python 的 `d.setdefault(key, deque())`：一次查找，拿到一个可以直接修改的引用。

## 对照 4：文件名清理（字符串处理）

**Python v1**：

```python
name = name.replace("\\", "/").split("/")[-1]
name = _ILLEGAL_CHARS.sub("", name)          # 正则删掉非法字符
name = name.strip().strip(". ")
if not name:
    return "unnamed"
```

**Rust v2**（`lanshare-proto/src/filename.rs`）：

```rust
let last = name.rsplit(['/', '\\']).next().unwrap_or("");
let cleaned: String = last
    .chars()
    .filter(|&c| !is_illegal(c) && !is_invisible_format(c))
    .collect();
let trimmed = cleaned.trim().trim_matches(|c| c == '.' || c == ' ');
if trimmed.is_empty() {
    return "unnamed".to_string();
}
```

- `.chars().filter(...).collect()` 很像 Python 的 `"".join(c for c in s if ok(c))`：同样是“遍历、过滤、收集”。Rust 的迭代器是惰性的，编译后和手写的循环一样快。
- `|&c| ...` 是闭包，相当于 Python 的 `lambda c: ...`。
- **按字节截断时不能切在汉字中间**：Python 的 `s[:n]` 按字符切，永远安全；Rust 的 `&s[..n]` 按字节切，切在一个汉字的 3 个字节中间就会 panic。所以 v2 写了 `truncate_bytes`，用 `is_char_boundary` 往回找安全的位置。

## 对照 5：并发，线程 + GIL vs tokio

| | Python v1 | Rust v2 |
|---|---|---|
| 服务器 | `ThreadingHTTPServer`：每个连接一个线程 | `axum` + `tokio`：每个连接一个轻量任务 |
| 同时真正在跑的 | 有 GIL，同一时刻只有一个线程执行 Python 代码 | 所有 CPU 核心都在干活 |
| 共享数据 | 想加锁就加，忘了加也能跑（但会出现竞态） | 不加 `Mutex` 就**编译不过** |
| 阻塞操作 | 直接写 | 放进 `spawn_blocking`，别卡住异步线程 |

最后一行是 Rust 最独特的地方：**“多个线程同时改同一份数据”这种 bug，编译器直接拒绝。**

## 对照 6：“函数调用了但还没执行”，Python 和 Rust 一样，JS 不一样

```python
async def fetch(url): ...
coro = fetch(u)          # Python：只是创建了协程对象，什么都没发生
await coro               # 这时才真正执行
```

```rust
let fut = client.get(u).send();   // Rust：只是创建了 Future，什么都没发生
fut.await;                        // 这时才真正执行
```

```js
const p = fetch(u);      // JavaScript：Promise 一创建就已经开始发请求了
```

第 5 章踩的那个坑（“看起来并发其实串行”），用 Python 的 `asyncio` 写也会踩：要么 `asyncio.gather`，要么 `asyncio.create_task`，Rust 里对应 `join_all` 和 `tokio::spawn`。

## 对照 7：测试

```python
class TextBoardTest(unittest.TestCase):
    def test_keeps_only_latest_messages(self):
        board = TextBoard()
        ...
        self.assertEqual(len(items), MAX_MESSAGES)
```

```rust
#[test]
fn newest_first_and_bounded() {
    let board = TextBoard::new();
    ...
    assert_eq!(list.len(), MAX_MESSAGES);
}
```

- 不需要类，一个函数前面加 `#[test]` 就是一个测试；
- `assert_eq!` 失败时会打印两边的值，和 `assertEqual` 一样；
- 单元测试可以直接写在源文件底部的 `#[cfg(test)] mod tests { ... }` 里，正式编译时整块被去掉。

## 小测验

**1. 下面这段 Python 搬到 Rust 时，`isinstance` 检查还需要吗？为什么？**
```python
def add(self, text):
    if not isinstance(text, str): raise ValueError()
```

<details><summary>答案</summary>

不需要。Rust 函数签名写着 `text: &str`，编译器保证传进来的一定是字符串，传别的类型根本编译不过。
</details>

**2. Python 的 `len("局域网")` 是 3，Rust 的 `"局域网".len()` 是多少？**

<details><summary>答案</summary>

9。Rust 的 `len()` 数的是 UTF-8 字节，一个汉字占 3 字节。要数字符，用 `"局域网".chars().count()`，结果是 3。
</details>

**3. Python 函数可能返回 `None`，在 Rust 里应该怎么表达？用的时候有什么不同？**

<details><summary>答案</summary>

返回 `Option<T>`。用之前**必须**先拆开（`match`、`if let`、`let ... else`、`?`），不拆开拿不到里面的值。所以“忘了判断 None”这种 bug 在 Rust 里编译不过。
</details>

**4. Python 的 `with open(path) as f:` 在 Rust 里对应什么？**

<details><summary>答案</summary>

`let f = File::open(path)?;`。`f` 离开作用域（所在的 `{}` 结束）时自动关闭文件。Rust 不需要专门的 `with` 语法，所有资源（文件、锁、网络连接）都遵循“离开作用域就释放”，这叫 RAII。
</details>

**5. 判断：Python 的 `asyncio` 协程和 Rust 的 Future 一样是“惰性”的。**

<details><summary>答案</summary>

**对。** 调用 `async def` 函数只是创建协程对象，`await`、`gather`、`create_task` 之后才会执行，Rust 的 Future 也一样。JavaScript 的 Promise 则是一创建就开始执行。
</details>
