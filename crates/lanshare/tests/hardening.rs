//! 独立安全审查（2026-10-02）发现的问题，每条一个回归测试。
//! I-1 口令在线猜测无总量上限；I-2 上传名额/磁盘泄漏；I-3 未认证请求体与连接耗尽内存；
//! M-2 握手接口可被跨站调用；M-5 块编号乘法溢出。

mod common;

use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::*;
use lanshare_proto as proto;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

fn wrong_pin(right: &str) -> &'static str {
    if right == "000000" { "111111" } else { "000000" }
}

// ---------- I-1：口令登录累计失败后暂停，只能在本机重新开启 ----------

#[tokio::test]
async fn pin_login_pauses_after_too_many_unfinished_attempts() {
    let s = start_with(Ipv4Addr::LOCALHOST, |c| {
        c.per_ip_limit = 1000;
        c.global_limit = 1000;
        c.pin_fail_limit = 6;
    })
    .await;
    let sid = server_id(&s).await;
    let pin = s.state.pin();
    for _ in 0..6 {
        assert_eq!(pair_pin(&s.base, wrong_pin(&pin), &sid).await.err().as_deref(), Some("wrong_pin"));
    }
    assert_eq!(pair_pin(&s.base, &pin, &sid).await.err().as_deref(), Some("423"), "暂停后连对的口令也不受理");
    // 两道防线分别检查：第一步 pake_start 本身就要拒绝（不给出能验证猜测的应答）
    let (_, msg) = proto::pake_start(&pin, &sid, rand_core::OsRng);
    let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &msg);
    let start = http()
        .post(format!("{}/api/pake/start", s.base))
        .json(&json!({ "cid": hex::encode(random_cid()), "msg": b64 }))
        .send()
        .await
        .unwrap();
    assert_eq!(start.status(), 423, "暂停后 pake_start 直接拒绝");

    let mut local = pair_qr(&s).await; // 扫码不受影响
    assert_eq!(local.rpc(json!({ "op": "info" })).await["pin_paused"], true);
    let resumed = local.rpc(json!({ "op": "pin_resume" })).await;
    assert_eq!(resumed["ok"], true);
    let new_pin = resumed["pin"].as_str().unwrap().to_string();
    assert_ne!(new_pin, pin, "重新开启时换新口令");
    assert_eq!(local.rpc(json!({ "op": "info" })).await["pin_paused"], false);
    assert!(pair_pin(&s.base, &new_pin, &sid).await.is_ok());
    assert!(s.logs().contains("已暂停口令登录"));
}

#[tokio::test]
async fn successful_logins_do_not_count_towards_the_pause() {
    let s = start_with(Ipv4Addr::LOCALHOST, |c| {
        c.per_ip_limit = 1000;
        c.global_limit = 1000;
        c.pin_fail_limit = 3;
    })
    .await;
    let sid = server_id(&s).await;
    for _ in 0..10 {
        assert!(pair_pin(&s.base, &s.state.pin(), &sid).await.is_ok());
    }
}

#[tokio::test]
async fn pin_resume_is_local_only() {
    let Some(lan_ip) = lanshare::netinfo::lan_ips().into_iter().next() else {
        eprintln!("本机没有局域网地址，跳过");
        return;
    };
    let s = start_with(Ipv4Addr::UNSPECIFIED, |_| {}).await;
    let base = format!("http://{lan_ip}:{}", s.port);
    let cid = random_cid();
    assert_eq!(hello(&base, &s.key(), &cid).await.status(), 200);
    let mut c = Client::from_secret(&base, cid, &proto::session_secret_from_key(&s.key(), &cid));
    let pin = s.state.pin();
    assert_eq!(c.rpc(json!({ "op": "pin_resume" })).await["error"], "only_local");
    assert_eq!(s.state.pin(), pin);
}

// ---------- M-2：握手接口只收 JSON，拒绝外站来源 ----------

#[tokio::test]
async fn handshake_requires_json_content_type() {
    let s = start().await;
    for path in ["/api/hello", "/api/pake/start", "/api/pake/finish"] {
        let r = http()
            .post(format!("{}{path}", s.base))
            .header("Content-Type", "text/plain;charset=UTF-8")
            .body(r#"{"cid":"00","msg":"AA=="}"#)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 415, "{path}：text/plain 是浏览器不用预检就能跨站发的类型");
    }
}

#[tokio::test]
async fn cross_site_origin_is_refused() {
    let s = start().await;
    let cid = random_cid();
    let tag = hex::encode(proto::hello_tag(&s.key(), &cid));
    let post = |origin: &'static str, cid: [u8; 16], tag: String| {
        let base = s.base.clone();
        async move {
            let mut req = http().post(format!("{base}/api/hello")).json(&json!({ "cid": hex::encode(cid), "tag": tag }));
            if !origin.is_empty() {
                req = req.header("Origin", origin);
            }
            req.send().await.unwrap().status().as_u16()
        }
    };
    assert_eq!(post("http://evil.example", cid, tag.clone()).await, 403);
    let same = Box::leak(format!("http://127.0.0.1:{}", s.port).into_boxed_str());
    assert_eq!(post(same, cid, tag).await, 200, "同源页面照常");
    let cid2 = random_cid();
    assert_eq!(post("", cid2, hex::encode(proto::hello_tag(&s.key(), &cid2))).await, 200, "没有 Origin（非浏览器客户端）照常");
}

// ---------- I-3：先验请求头再读正文；正文、连接都有超时和上限 ----------

async fn raw_request(port: u16, head: &str, body: &[u8]) -> TcpStream {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    stream.write_all(head.as_bytes()).await.unwrap();
    stream.write_all(body).await.unwrap();
    stream
}

async fn read_status(stream: &mut TcpStream, within: Duration) -> Option<String> {
    let mut buf = vec![0u8; 256];
    match tokio::time::timeout(within, stream.read(&mut buf)).await {
        Ok(Ok(n)) if n > 0 => String::from_utf8_lossy(&buf[..n]).lines().next().map(str::to_string),
        _ => None,
    }
}

#[tokio::test]
async fn unknown_session_is_rejected_before_the_body_arrives() {
    let s = start().await;
    let head = |length: usize| {
        format!(
            "POST /api/up/x/0 HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Type: application/octet-stream\r\n\
             X-LS-Sid: {}\r\nX-LS-Ctr: 1\r\nContent-Length: {length}\r\n\r\n",
            s.port,
            hex::encode(random_cid())
        )
    };
    // 正文一个字节都不发：服务端只凭请求头就要给出答复
    let mut stream = raw_request(s.port, &head(proto::CHUNK + proto::TAG_LEN), b"").await;
    let status = read_status(&mut stream, Duration::from_secs(3)).await;
    assert_eq!(status.as_deref(), Some("HTTP/1.1 401 Unauthorized"), "不该先把 1 MiB 正文收进内存再查会话");
    let mut stream = raw_request(s.port, &head(proto::CHUNK + proto::TAG_LEN + 1), b"").await;
    let status = read_status(&mut stream, Duration::from_secs(3)).await;
    assert_eq!(status.as_deref(), Some("HTTP/1.1 413 Payload Too Large"), "声明的长度超限也不用等正文");
}

#[tokio::test]
async fn stalled_body_times_out() {
    let s = start_with(Ipv4Addr::LOCALHOST, |c| c.body_timeout = Duration::from_millis(500)).await;
    let c = pair_qr(&s).await;
    let head = format!(
        "POST /api/rpc HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Type: application/octet-stream\r\n\
         X-LS-Sid: {}\r\nX-LS-Ctr: 1\r\nContent-Length: 1000\r\n\r\n",
        s.port,
        hex::encode(c.cid)
    );
    let mut stream = raw_request(s.port, &head, &[0u8; 10]).await; // 只发 10 字节就停
    let status = read_status(&mut stream, Duration::from_secs(3)).await;
    assert_eq!(status.as_deref(), Some("HTTP/1.1 408 Request Timeout"));
}

#[tokio::test]
async fn idle_connections_are_closed() {
    let s = start_with(Ipv4Addr::LOCALHOST, |c| c.idle_timeout = Duration::from_millis(300)).await;
    let mut stream = TcpStream::connect(("127.0.0.1", s.port)).await.unwrap();
    let mut buf = [0u8; 16];
    let started = Instant::now();
    let closed = tokio::time::timeout(Duration::from_secs(3), stream.read(&mut buf)).await;
    assert!(matches!(closed, Ok(Ok(0)) | Ok(Err(_))), "一直不说话的连接应被关掉：{closed:?}");
    assert!(started.elapsed() < Duration::from_secs(2));
}

/// 从指定的本机回环地址发请求（Windows 和 Linux 都接受 127.0.0.0/8 里的任何源地址）。
fn http_from(ip: [u8; 4]) -> reqwest::Client {
    reqwest::Client::builder().no_proxy().local_address(std::net::IpAddr::from(ip)).build().unwrap()
}

// 复验 N-1：连接名额按来源 IP 分开，一台机器占不满所有名额
#[tokio::test]
async fn connections_per_ip_are_capped() {
    let s = start_with(Ipv4Addr::LOCALHOST, |c| c.max_connections_per_ip = 2).await;
    let a = TcpStream::connect(("127.0.0.1", s.port)).await.unwrap();
    let _b = TcpStream::connect(("127.0.0.1", s.port)).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let url = format!("{}/api/server", s.base);
    let started = Instant::now();
    let refused = http().get(&url).timeout(Duration::from_secs(3)).send().await;
    assert!(refused.is_err(), "同一个 IP 超过上限的连接直接拒绝");
    assert!(started.elapsed() < Duration::from_secs(2), "拒绝要快，不能让人干等");
    let other = http_from([127, 0, 0, 2]).get(&url).timeout(Duration::from_secs(3)).send().await.unwrap();
    assert_eq!(other.status(), 200, "别的 IP 不受影响");
    drop(a);
    tokio::time::sleep(Duration::from_millis(200)).await;
    let ok = http().get(&url).timeout(Duration::from_secs(3)).send().await.unwrap();
    assert_eq!(ok.status(), 200, "有连接断开后就能进来");
}

// 复验 N-1：请求头必须在时限内收完，慢慢挤牙膏也不行
#[tokio::test]
async fn headers_must_arrive_in_time() {
    let s = start_with(Ipv4Addr::LOCALHOST, |c| c.header_timeout = Duration::from_millis(400)).await;
    let mut stream = TcpStream::connect(("127.0.0.1", s.port)).await.unwrap();
    let started = Instant::now();
    let mut closed = false;
    for byte in b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Slow: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".iter().cycle().take(40) {
        if stream.write_all(&[*byte]).await.is_err() {
            closed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await; // 每 50 毫秒一个字节：空闲计时永远不会到期
        let mut buf = [0u8; 64];
        if let Ok(Ok(n)) = tokio::time::timeout(Duration::from_millis(1), stream.read(&mut buf)).await {
            closed = n == 0 || String::from_utf8_lossy(&buf[..n]).starts_with("HTTP/1.1 408");
            if closed {
                break;
            }
        }
    }
    assert!(closed, "请求头 2 秒都没发完，应该早就被断开");
    assert!(started.elapsed() < Duration::from_millis(1500));
}

// 第三轮复验 R3-1：握手接口的正文也要限时（不然一个连接每隔一会儿发一个字节，能挂几十个小时）
#[tokio::test]
async fn handshake_body_must_arrive_in_time() {
    let s = start_with(Ipv4Addr::LOCALHOST, |c| c.handshake_timeout = Duration::from_millis(500)).await;
    for path in ["/api/hello", "/api/pake/start", "/api/pake/finish"] {
        let head = format!(
            "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Type: application/json\r\nContent-Length: 100\r\n\r\n",
            s.port
        );
        let mut stream = raw_request(s.port, &head, br#"{"cid":"#).await; // 只发了开头就停
        let status = read_status(&mut stream, Duration::from_secs(3)).await;
        assert_eq!(status.as_deref(), Some("HTTP/1.1 408 Request Timeout"), "{path}");
    }
}

// 复验 N-1：本机界面有自己的名额，局域网把名额占满也打不开不了本机界面
#[tokio::test]
async fn loopback_keeps_its_own_pool() {
    let Some(lan_ip) = lanshare::netinfo::lan_ips().into_iter().next() else {
        eprintln!("本机没有局域网地址，跳过");
        return;
    };
    let s = start_with(Ipv4Addr::UNSPECIFIED, |c| {
        c.max_connections = 2;
        c.max_connections_per_ip = 100;
    })
    .await;
    let _a = TcpStream::connect((lan_ip, s.port)).await.unwrap();
    let _b = TcpStream::connect((lan_ip, s.port)).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let lan = http().get(format!("http://{lan_ip}:{}/api/server", s.port)).timeout(Duration::from_secs(3)).send().await;
    assert!(lan.is_err(), "局域网的名额满了");
    let local = http().get(format!("{}/api/server", s.base)).timeout(Duration::from_secs(3)).send().await.unwrap();
    assert_eq!(local.status(), 200, "本机界面照常能用（包括“重新开启口令登录”）");
}

// 复验 N-2：读正文的名额按来源 IP 分开
#[tokio::test]
async fn body_reads_per_ip_are_capped() {
    let s = start_with(Ipv4Addr::LOCALHOST, |c| c.max_bodies_per_ip = 1).await;
    let mut c = pair_qr(&s).await;
    // 127.0.0.1 上挂一个只发了一半正文的请求，占住这个 IP 的名额
    let head = format!(
        "POST /api/rpc HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Type: application/octet-stream\r\n\
         X-LS-Sid: {}\r\nX-LS-Ctr: 4000\r\nContent-Length: 1000\r\n\r\n",
        s.port,
        hex::encode(c.cid)
    );
    let _stalled = raw_request(s.port, &head, &[0u8; 10]).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let (ctr, body) = c.seal("/api/rpc", br#"{"op":"texts"}"#);
    let (status, _) = c.post_raw("/api/rpc", ctr, body).await;
    assert_eq!(status, 503, "同一 IP 的名额被占着：立刻告诉客户端稍后再试");
    c.http = http_from([127, 0, 0, 2]);
    assert_eq!(c.rpc(json!({ "op": "texts" })).await["ok"], true, "别的 IP 照常");
}

// 复验 N-4：暂停前的并发尝试不能超过上限（多线程运行时：请求真的同时在跑）
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn pause_cannot_be_overshot_by_concurrent_attempts() {
    let s = start_with(Ipv4Addr::LOCALHOST, |c| {
        c.per_ip_limit = 1000;
        c.global_limit = 1000;
        c.pin_fail_limit = 6;
        c.max_connections_per_ip = 100; // 20 个并发请求都从 127.0.0.1 来
    })
    .await;
    let sid = server_id(&s).await;
    let tasks: Vec<_> = (0..20)
        .map(|_| {
            let (base, sid) = (s.base.clone(), sid.clone());
            tokio::spawn(async move {
                let (_, msg) = proto::pake_start("000000", &sid, rand_core::OsRng);
                let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &msg);
                http()
                    .post(format!("{base}/api/pake/start"))
                    .json(&json!({ "cid": hex::encode(random_cid()), "msg": b64 }))
                    .send()
                    .await
                    .unwrap()
                    .status()
                    .as_u16()
            })
        })
        .collect();
    let mut answered = 0;
    for t in tasks {
        if t.await.unwrap() == 200 {
            answered += 1;
        }
    }
    assert_eq!(answered, 6, "每开启一轮，能拿到应答（也就是能验证一次猜测）的最多 6 次");
}

// ---------- I-2：上传名额、过期清理、磁盘余量 ----------

fn part_files(s: &TestServer) -> usize {
    std::fs::read_dir(s.share())
        .map(|d| d.filter_map(Result::ok).filter(|e| e.file_name().to_string_lossy().ends_with(".part")).count())
        .unwrap_or(0)
}

#[tokio::test]
async fn uploads_per_session_are_capped() {
    let s = start_with(Ipv4Addr::LOCALHOST, |c| c.max_uploads_per_session = 2).await;
    let mut a = pair_qr(&s).await;
    for _ in 0..2 {
        assert_eq!(a.rpc(json!({ "op": "upload_begin", "name": "x", "size": 3 })).await["ok"], true);
    }
    assert_eq!(a.rpc(json!({ "op": "upload_begin", "name": "x", "size": 3 })).await["error"], "too_many_uploads");
    let mut b = pair_qr(&s).await;
    assert_eq!(b.rpc(json!({ "op": "upload_begin", "name": "y", "size": 3 })).await["ok"], true, "别的设备不受影响");
}

#[tokio::test]
async fn concurrent_begins_never_exceed_the_global_cap() {
    let s = start_with(Ipv4Addr::LOCALHOST, |c| {
        c.max_uploads = 5;
        c.max_uploads_per_session = 100;
        // 这里专门测上传名额：20 个并发请求都从 127.0.0.1 来，先把每 IP 的连接/正文上限放开
        c.max_connections_per_ip = 100;
        c.max_bodies_per_ip = 100;
    })
    .await;
    let mut c = pair_qr(&s).await;
    let plain = json!({ "op": "upload_begin", "name": "x", "size": 3 }).to_string();
    let sealed: Vec<(u64, Vec<u8>)> = (0..20).map(|_| c.seal("/api/rpc", plain.as_bytes())).collect();
    let c = Arc::new(c);
    let tasks: Vec<_> = sealed
        .into_iter()
        .map(|(ctr, body)| {
            let c = c.clone();
            tokio::spawn(async move { (ctr, c.post_raw("/api/rpc", ctr, body).await) })
        })
        .collect();
    let mut ok = 0;
    for t in tasks {
        let (ctr, (status, body)) = t.await.unwrap();
        assert_eq!(status, 200);
        let plain = proto::open(&c.keys.s2c, ctr, &proto::resp_aad("/api/rpc"), &body).unwrap();
        let reply: serde_json::Value = serde_json::from_slice(&plain).unwrap();
        if reply["ok"] == true {
            ok += 1;
        }
    }
    assert_eq!(ok, 5, "并发时也不能超过全局名额");
    assert_eq!(part_files(&s), 5);
}

#[tokio::test]
async fn idle_uploads_are_reaped() {
    let s = start_with(Ipv4Addr::LOCALHOST, |c| c.upload_idle = Duration::from_millis(300)).await;
    let mut c = pair_qr(&s).await;
    let id = c.rpc(json!({ "op": "upload_begin", "name": "x", "size": 3 })).await["upload_id"].as_str().unwrap().to_string();
    assert_eq!(part_files(&s), 1);
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(part_files(&s), 0, "没人管的上传要自动清理，临时文件和预占的空间一起释放");
    assert_eq!(c.rpc(json!({ "op": "upload_status", "upload_id": id })).await["error"], "not_found");
}

#[tokio::test]
async fn active_uploads_are_not_reaped() {
    let s = start_with(Ipv4Addr::LOCALHOST, |c| c.upload_idle = Duration::from_millis(600)).await;
    let mut c = pair_qr(&s).await;
    let size = 3 * proto::CHUNK as u64;
    let id = c.rpc(json!({ "op": "upload_begin", "name": "big.bin", "size": size })).await["upload_id"]
        .as_str()
        .unwrap()
        .to_string();
    for index in 0..3 {
        tokio::time::sleep(Duration::from_millis(400)).await; // 每块间隔都小于过期时间，总时长超过它
        assert_eq!(c.upload_chunk(&id, index, &vec![7u8; proto::CHUNK]).await["ok"], true);
    }
    assert_eq!(c.rpc(json!({ "op": "upload_finish", "upload_id": id })).await["ok"], true);
}

// 复验 N-5：只查进度、不传块，不能让上传一直占着预留的空间
#[tokio::test]
async fn status_polling_does_not_keep_an_upload_alive() {
    let s = start_with(Ipv4Addr::LOCALHOST, |c| c.upload_idle = Duration::from_millis(600)).await;
    let mut c = pair_qr(&s).await;
    let id = c.rpc(json!({ "op": "upload_begin", "name": "x", "size": 3 })).await["upload_id"].as_str().unwrap().to_string();
    let mut last = json!(null);
    for _ in 0..6 {
        tokio::time::sleep(Duration::from_millis(300)).await;
        last = c.rpc(json!({ "op": "upload_status", "upload_id": id })).await;
    }
    assert_eq!(last["error"], "not_found", "1.8 秒没有新块，早该过期了");
}

#[tokio::test]
#[cfg(windows)]
async fn begin_keeps_a_disk_reserve() {
    let s = start_with(Ipv4Addr::LOCALHOST, |c| c.disk_reserve = 1 << 62).await;
    let mut c = pair_qr(&s).await;
    let r = c.rpc(json!({ "op": "upload_begin", "name": "x", "size": 1 })).await;
    assert_eq!(r["error"], "disk_full", "上传后剩余空间低于保留量就拒绝，不能把磁盘占满");
    assert_eq!(part_files(&s), 0);
}

// ---------- M-5：块编号超大不溢出 ----------

#[tokio::test]
async fn huge_chunk_index_is_bad_index() {
    let s = start().await;
    std::fs::create_dir_all(s.share()).unwrap();
    std::fs::write(s.share().join("a.txt"), b"hello").unwrap();
    let mut c = pair_qr(&s).await;
    let id = c.find_file("a.txt").await["id"].as_str().unwrap().to_string();
    assert_eq!(c.download_chunk(&id, usize::MAX, json!({})).await.err().as_deref(), Some("bad_index"));
}
