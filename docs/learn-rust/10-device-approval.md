# 第 10 章　加一道门：新设备要在电脑上点“允许”

## 这一步要解决什么

前 9 章做完，安全说明里有一条诚实边界：**拿到二维码或口令的人，就是可信用户**。二维码里就带着密钥 K，谁扫了谁就能连上来。

一位用户读到这里问：“那二维码要是泄露了，是不是就完蛋了？”泄露的方式其实很平常：二维码被拍进照片、被截图发到群里，或者留在扫码 App 的历史记录里。拿到的人在自己的手机上一扫，就能看文件、传文件，电脑前的人还不知道。

这一章加一道门：**局域网里第一次连上来的设备，要电脑前的人点“允许”才能用。**

做之前，先说清楚这道门能挡什么、挡不住什么：

| | 结果 |
|---|---|
| 拿到二维码或口令，在**别的设备**上连接 | 握手能成功，但什么都拿不到；电脑上冒出一条不认识的设备，点“拒绝” |
| 猜中了口令 | 同上：猜中也要电脑上点“允许” |
| 拿到二维码，**又在同一 WiFi 里抓包** | 挡不住：能解密已经放行的设备的流量（会话密钥由 K 和明文的会话号派生）。要挡住，得在握手里加一次性的密钥交换，要改协议，这一章不做 |
| 冒用已经放行的设备的 IP | 挡不住：门按 IP 记。冒用 IP 属于主动攻击，本来就不在防护范围。想用偷看到的会话发请求，也得走这一步 |
| 拿到二维码，反复连接捣乱 | 占满 8 个待确认名额、每 30 秒让主界面弹一次；反复握手开新会话，把别人的会话挤掉：会打扰，不会泄密。开新会话有上限（每个 IP 每分钟 30 个，全局 300 个），所以耗不光内存 |

## 概念 1：先想清楚“记住谁”

“允许”以后要记住点什么，下次就不用再点。有三个候选：

1. **按会话记**。最省事，但手机每次打开、刷新页面，都会用存下的 K 重新握手，换一个新的会话号（第 6 章的 `resume()`）。刷新一次就要再点一次，没法用。
2. **给设备发一个“令牌”**，手机存起来，每次握手带上。听起来像正经做法，但令牌要么明文放在握手里，要么放在用 K 派生的信道里：持有 K 又能抓包的人照样拿得到。它没有比下一种更安全，还要改协议。
3. **按 IP 记**。某个 IP 被允许以后，这次运行内它的所有会话都放行；程序重启就清空，和 K、口令同一个生命周期。

选了第 3 种。代价写进安全说明：和放行过的设备共用一个 IP（比如都在同一个路由器的 NAT 后面）的设备会一起被放行；家里的局域网里每台设备有自己的 IP，一般遇不到。

> 设计安全功能时，先问“攻击者拿到什么就能绕过”。如果两种做法被绕过的条件一样，选简单的那个。

## 概念 2：门放在哪，决定了要改多少东西

能放门的地方有两个：

- **握手**（`/api/hello`、口令配对）。握手接口是协议的一部分，改它就要同时改 `lanshare-proto`、WASM、`proto.js` 和已知向量（第 2、3 章）。
- **加密信道**（`channel.rs` 的 `sealed`）。每个加密请求解密成功以后、交给 RPC 之前，先问一句“这个 IP 放行了吗”。

放在信道里，握手和协议一个字节都不用动。还有一个好处：只有握手成功、能正确加密的请求才走得到门口。不知道 K 也不知道口令的人，连“待确认”都登记不上，没法往电脑上刷一堆假请求。

```rust
// channel.rs 的 sealed：解密成功之后
let local = peer.ip().is_loopback();
// 局域网里的新设备：电脑上点了“允许”才放行。分块上传下载直接 403：没放行的设备还拿不到文件列表和上传号
if !local && st.approve_new_devices && let Some(reply) = gate(&st, peer.ip(), &parts.headers) {
    return if path == "/api/rpc" { sealed_response(&opened, &path, &reply) } else { StatusCode::FORBIDDEN.into_response() };
}
```

“等确认”的回复也是加密的，验证码只有那台手机看得到。

这一次所有改动都是**新增的行**，没改原有代码（`git diff` 里没有一行减号）。原来的 121 个测试一个都没改：测试工具默认把新设备确认关掉（`approve_new_devices = false`），只有新写的 `tests/approval.rs` 打开它。正式运行用的 `Config::new` 默认是打开的，另有一个测试守着这一点。

## 概念 3：枚举可以带数据

门的判断结果有四种：放行、等确认（带一个验证码）、拒绝、等的设备太多。Rust 的枚举每一种都可以带不同的数据：

```rust
pub(crate) enum Gate {
    Allow,
    /// 等电脑上确认。`new`：这台设备刚刚登记。
    Wait { code: String, new: bool },
    Reject,
    /// 待确认的设备太多了。
    Busy,
}
```

用的时候 `match`，每一种都必须处理，漏了编译不过：

```rust
let reply = match gate {
    Gate::Allow => return None,
    Gate::Wait { code, new } => {
        if new {
            st.log(&format!("新设备请求连接：{ip}（{device}），等电脑上确认"));
        }
        json!({ "ok": false, "error": "pending_approval", "code": code })
    }
    Gate::Reject => json!({ "ok": false, "error": "rejected" }),
    Gate::Busy => json!({ "ok": false, "error": "approval_busy" }),
};
```

只想问“是不是某一种”的时候，用 `matches!` 宏，`..` 表示其余字段不关心：

```rust
let pop = matches!(gate, Gate::Wait { new: true, .. }) && approvals.should_pop(now);
```

Python 的 `Enum` 每个成员只是一个常量，带不了“这一次的验证码”。通常的写法是返回一个元组 `("wait", "4821")`，或者为每种结果写一个 dataclass。Python 3.10 的 `match` 能解构它们，但不会检查你有没有漏掉哪一种。

## 概念 4：把“现在几点”当参数传进来

门里有好几个和时间有关的规则：待确认的设备 60 秒没再问就撤掉，被拒绝的 IP 2 分钟内直接拒绝，最多 30 秒弹一次窗口。要测试它们，难道真的等 2 分钟？

办法是：`Approvals` 的方法都不自己去看表，而是由调用的人把 `now` 传进来：

```rust
pub(crate) fn gate(&mut self, ip: IpAddr, device: &'static str, now: Instant) -> Gate {
```

真实运行时传 `Instant::now()`；测试里传一个“假装过了 2 分钟”的时刻，一瞬间就测完了：

```rust
let later = t + REJECT_HOLD + Duration::from_secs(1);
assert!(matches!(a.gate(ip(9), "安卓设备", later), Gate::Wait { new: true, .. }));
```

`Instant` 加 `Duration` 得到另一个 `Instant`，两个 `Instant` 相减（`duration_since`）得到 `Duration`，类型分得很清楚：时刻和时长不会搞混。

Python 里同样的问题，常见做法是用 `unittest.mock.patch("time.monotonic")` 或者 freezegun 这类库把时间“冻住”。能用，但是改的是全局的东西，测试之间容易互相影响。把时间当参数传进来，两种语言都适用，只是 Rust 里这么写更自然。

还有一个小工具：`Option::is_none_or`（Rust 1.82 起）。“主界面从来没来问过，或者上次问已经是 6 秒以前”：

```rust
let away = self.desktop_seen.is_none_or(|t| now.duration_since(t) > DESKTOP_AWAY);
```

`None`（从来没有）直接算 `true`，`Some(t)` 才去看闭包。

## 概念 5：锁里只算，出了锁再做事

新设备登记的时候，如果主界面没开着，要把它弹出来。弹出来就是启动一个 Edge 进程，要花几十毫秒。不能拿着 `approvals` 的锁去做这件事：这期间别的设备的每个请求都要等这把锁。

写法是用一个块表达式，把“要在锁里做的事”圈起来，算出结果，块一结束锁就释放了：

```rust
let (gate, pop) = {
    let mut approvals = st.approvals.lock().expect("锁不会中毒");
    let gate = approvals.gate(ip, device, now);
    let pop = matches!(gate, Gate::Wait { new: true, .. }) && approvals.should_pop(now);
    (gate, pop)
};
// 这里锁已经释放了
if pop && let Some(desktop) = &st.desktop {
    desktop.open_app(&st.local_entry_url());
}
```

Python 里是 `with lock:` 块。区别在于 Python 的 `with` 块不能“返回一个值”，要在块外先声明变量、块里赋值；Rust 的大括号本身就是一个表达式，最后一行就是它的值。

## 概念 6：`&'static str`，不用分配的字符串

设备类型从 User-Agent 里粗分出来，结果只可能是几个固定的词。函数返回 `&'static str`：指向程序里写死的字符串常量，活得和程序一样长，不用每次分配一个 `String`：

```rust
pub(crate) fn device_label(user_agent: &str) -> &'static str {
    // 顺序有讲究：安卓的 User-Agent 里也有 Linux，iPhone 的里也有 Mac OS X
    const KINDS: [(&str, &str); 7] = [
        ("iPhone", "iPhone"),
        ("iPad", "iPad"),
        ("Android", "安卓设备"),
        // ……
    ];
    KINDS.iter().find(|(needle, _)| user_agent.contains(needle)).map_or("未知设备", |(_, label)| label)
}
```

`'static` 是一个生命周期标注，意思是“这个引用永远有效”。正因为它永远有效，`Waiting` 结构体可以直接存 `device: &'static str`，不用操心它指向的东西什么时候被释放。

User-Agent 谁都可以随便填，所以它只用来帮电脑前的人认设备，不当身份用。真正用来核对的是 4 位验证码。

## 验证码：怎么生成均匀的 4 位数

第 9 章的 `random_pin()` 生成均匀分布的 6 位数字。取它的前 4 位就行：000000～999999 一共一百万个数，前 4 位是 0000～9999，每一种正好对应 100 个 6 位数，所以也是均匀的。

```rust
// 6 位口令是均匀分布的，取前 4 位也是均匀分布的
let code = super::random_pin()[..4].to_string();
```

## 浏览器这边

- **手机**：进入主界面前先问一次 `info`。回答是 `pending_approval`，就显示“等电脑上点‘允许’”和验证码，每 1.5 秒再问一次；变成正常的回答就进去；回答是 `rejected`，就忘掉存下的 K，回到登录页。
- **电脑**：主界面本来每 2 秒刷新一次文件列表，现在多问一句 `approvals`。有设备在等，就在左栏最上面显示一张卡片（设备类型、IP、验证码、“允许”“拒绝”），窗口标题变成“（1）有设备等确认”，任务栏上也看得到。

用口令配对的手机，原来配对后马上经加密信道拿 K；现在要等放行以后才拿得到，所以进主界面时再记一次（`proto.js` 新增的 `rememberKey`）。

## 测试：用自己的局域网地址扮演“另一台手机”

服务端判断“本机还是局域网”看的是对方的 IP。测试里怎么造一台“局域网设备”？让服务端监听 `0.0.0.0`，测试客户端连本机的局域网地址（比如 `192.168.1.3`），服务端看到的对方 IP 就不是回环地址了。本机没有局域网地址时，这几个测试打印“跳过”。

```rust
async fn lan_phone() -> Option<(TestServer, Ipv4Addr, Client)> {
    let lan_ip = lanshare::netinfo::lan_ips().into_iter().next()?;
    let s = start_with(Ipv4Addr::UNSPECIFIED, |c| c.approve_new_devices = true).await;
    let phone = scan(&s, lan_ip).await;
    Some((s, lan_ip, phone))
}
```

新增的测试覆盖了：等确认时什么都拿不到、验证码电脑和手机一致、放行后刷新页面不用再点、口令配对也要放行、拒绝后重新扫码也不行、局域网设备不能替电脑做决定、主界面没开时才弹窗、本机不用确认、正式运行默认打开。另外在 `approval.rs` 里有 6 个单元测试，用“假时间”测各种时限。

最后用真实的 exe 和两个浏览器窗口（一个走局域网地址扮演手机，一个走 127.0.0.1 扮演电脑）把“允许”和“拒绝”两条路各走了一遍。

## 真实踩坑

1. **改了一行 `use`，“只加不改”就破了。** 门的函数要用 `IpAddr`，顺手把 `use std::net::SocketAddr;` 改成了 `use std::net::{IpAddr, SocketAddr};`。代码没问题，但这一行变了，`git diff` 里出现了一个减号。这个文件里 `BodySlot` 本来就写的是完整路径 `std::net::IpAddr`，照着写，原来那行就不用动。
2. **第一版改了两个旧测试，第二版一个都不改。** 原来有两个测试用局域网地址扮演别的设备，检查它调不了“在资源管理器中显示”这类本机专用的操作。加了门以后，它们先被门挡住，拿到的是“等确认”。第一版在这两个测试里各插了一行“先在电脑上放行”，结果又撞上另一个断言：“假桌面一次都不能被调用”。放行之前主界面没开着，服务端弹了一次窗，多了一条调用记录。后来想明白了：这两个测试测的是别的事，不该被新功能牵连。于是改成测试工具默认关掉确认，只有新功能自己的测试打开；再加一个测试守着“正式运行默认是打开的”，免得哪天默认值被悄悄改掉。**新功能的开关，正式运行默认开，测试里按需开。**
3. **断言写错了地方。** “本机连接不会登记成新设备”，一开始写成“日志里不出现 127.0.0.1”，结果失败：握手本来就会记一条“新设备已配对（扫码）：127.0.0.1”。要检查的其实是“新设备请求连接”这句话不出现。**断言要对准你想证明的那件事**，不要找一个看起来相关的替代品。

## 和 Python 对照

| 要做的事 | Python | Rust |
|---|---|---|
| 判断结果有几种、各带不同数据 | 元组 `("wait", code)` 或几个 dataclass | 带数据的枚举 `Gate::Wait { code, new }` |
| 处理每一种结果 | `if/elif` 或 3.10 的 `match`，漏了不报错 | `match`，漏了编译不过 |
| 只问“是不是某一种” | `isinstance(r, Wait) and r.new` | `matches!(gate, Gate::Wait { new: true, .. })` |
| 测试里控制时间 | `mock.patch("time.monotonic")`、freezegun | 把 `now: Instant` 当参数传进来 |
| 锁里算出结果、出锁再用 | 块外先声明变量，`with lock:` 里赋值 | `let (a, b) = { let guard = lock(); ...; (a, b) };` |
| 返回固定的几个词之一 | 直接返回字符串字面量 | `&'static str` |

## 小测验

**1. 为什么不按会话记“允许过”？**

<details><summary>答案</summary>

手机每次打开、刷新页面，都会用存下的 K 重新握手，换一个新的会话号。按会话记，刷新一次就要在电脑上再点一次。按 IP 记，这次运行内同一台设备就不用再点。
</details>

**2. 为什么把门放在加密信道里，而不是握手里？**

<details><summary>答案</summary>

握手是协议的一部分，改它要同时改 proto、WASM、proto.js 和已知向量。放在信道里，握手和协议都不用动；而且只有握手成功、能正确加密的请求才走得到门口，没有 K 也没有口令的人连“待确认”都登记不上。
</details>

**3. 拿到二维码、又能在同一 WiFi 里抓包的人，这道门挡得住吗？**

<details><summary>答案</summary>

挡不住偷看。会话密钥由 K 和明文的会话号派生，持有 K 又看得到会话号，就能算出已经放行的设备的会话密钥，解密它的流量。想用这个会话发请求，门看的是他自己的 IP，还是会被当成新设备，除非他连 IP 也冒用（主动攻击）。要挡住，得在握手里加一次性的密钥交换（前向保密），要改协议。安全说明里照实写了这一条。
</details>

**4. 下面这段有什么问题？**
```rust
let mut approvals = st.approvals.lock().expect("锁不会中毒");
if let Gate::Wait { new: true, .. } = approvals.gate(ip, device, now) {
    desktop.open_app(&url); // 启动一个 Edge 进程
}
```

<details><summary>答案</summary>

拿着锁去启动进程。启动进程要几十毫秒，这期间所有局域网设备的请求都在等这把锁。应该在锁里只算出“要不要弹窗”，用块表达式让锁先释放，再去弹窗。
</details>

**5. `random_pin()` 生成均匀的 6 位数，为什么取前 4 位也是均匀的？取前 5 位呢？**

<details><summary>答案</summary>

前 4 位的每一种取值（0000～9999）正好对应 100 个 6 位数，次数一样多，所以均匀。前 5 位同理，每种对应 10 个，也均匀。只要 10 的幂能整除，取前几位都是均匀的。
</details>
