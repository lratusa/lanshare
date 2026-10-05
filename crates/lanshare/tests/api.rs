//! 服务端核心：静态资源、Host 校验、扫码握手、口令配对、加密信道、RPC、攻击用例。

mod common;

use std::net::Ipv4Addr;

use common::*;
use lanshare_proto as proto;
use serde_json::json;

// ---------- 静态资源与响应头 ----------

#[tokio::test]
async fn static_assets_are_public_with_security_headers() {
    let s = start().await;
    for (path, content_type) in [
        ("/", "text/html"),
        ("/app.js", "text/javascript"),
        ("/proto.js", "text/javascript"),
        ("/style.css", "text/css"),
        ("/lanshare.wasm", "application/wasm"),
        ("/favicon.ico", "image/x-icon"),
    ] {
        let resp = http().get(format!("{}{path}", s.base)).send().await.unwrap();
        assert_eq!(resp.status(), 200, "{path}");
        let h = resp.headers().clone();
        assert!(h["content-type"].to_str().unwrap().starts_with(content_type), "{path}");
        let csp = h["content-security-policy"].to_str().unwrap();
        assert!(csp.contains("default-src 'none'") && csp.contains("script-src 'self' 'wasm-unsafe-eval'"), "{csp}");
        assert_eq!(h["x-content-type-options"], "nosniff");
        assert_eq!(h["referrer-policy"], "no-referrer");
        assert_eq!(h["cache-control"], "no-store");
        let body = resp.bytes().await.unwrap();
        let key_hex = hex::encode(s.key());
        assert!(!String::from_utf8_lossy(&body).contains(&key_hex), "静态资源里不能有密钥");
    }
}

#[tokio::test]
async fn server_id_is_public_but_nothing_else() {
    let s = start().await;
    let resp = http().get(format!("{}/api/server", s.base)).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let v: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(v["server_id"], s.state.server_id());
    assert!(v.get("pin").is_none() && v.get("key").is_none());
}

#[tokio::test]
async fn unknown_route_is_404() {
    let s = start().await;
    assert_eq!(http().get(format!("{}/nope", s.base)).send().await.unwrap().status(), 404);
}

#[tokio::test]
async fn wrong_host_is_rejected() {
    let s = start().await;
    let resp = http().get(format!("{}/", s.base)).header("Host", format!("evil.example:{}", s.port)).send().await.unwrap();
    assert_eq!(resp.status(), 421, "DNS rebinding：别的域名指到本机也不认");
    let resp = http().get(format!("{}/", s.base)).header("Host", format!("localhost:{}", s.port)).send().await.unwrap();
    assert_eq!(resp.status(), 200);
}

// ---------- 扫码握手 ----------

#[tokio::test]
async fn qr_pairing_then_info() {
    let s = start().await;
    let mut c = pair_qr(&s).await;
    let info = c.rpc(json!({ "op": "info" })).await;
    assert_eq!(info["ok"], true);
    assert_eq!(info["pin"], s.state.pin());
    assert_eq!(info["server_id"], s.state.server_id());
    assert_eq!(info["local"], true);
    assert_eq!(info["chunk"], proto::CHUNK);
    assert!(info["folder"].as_str().unwrap().ends_with("share"));
    assert!(info["qr_svg"].as_str().unwrap().contains("<svg"));
    let key_b64 = base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, s.key());
    assert_eq!(info["key"], key_b64, "已配对设备经加密信道拿到 K，下次打开页面可以直接握手");
    let urls = info["urls"].as_array().unwrap();
    assert!(!urls.is_empty());
}

#[tokio::test]
async fn wrong_hello_tag_is_rejected_and_rate_limited() {
    let s = start().await;
    let wrong_key = [9u8; 32];
    for _ in 0..5 {
        assert_eq!(hello(&s.base, &wrong_key, &random_cid()).await.status(), 403);
    }
    assert_eq!(hello(&s.base, &wrong_key, &random_cid()).await.status(), 429, "连续失败 5 次锁定");
    assert_eq!(hello(&s.base, &s.key(), &random_cid()).await.status(), 429, "锁定期间正确的也不行");
}

#[tokio::test]
async fn malformed_hello_is_400() {
    let s = start().await;
    for body in [json!({}), json!({ "cid": "zz", "tag": "00" }), json!({ "cid": "00", "tag": "00" })] {
        let resp = http().post(format!("{}/api/hello", s.base)).json(&body).send().await.unwrap();
        assert_eq!(resp.status(), 400, "{body}");
    }
}

#[tokio::test]
async fn replayed_hello_does_not_reset_the_session() {
    let s = start().await;
    let mut c = pair_qr(&s).await;
    let (ctr, body) = c.seal("/api/rpc", br#"{"op":"texts"}"#);
    assert_eq!(c.post_raw("/api/rpc", ctr, body.clone()).await.0, 200);
    assert_eq!(hello(&s.base, &s.key(), &c.cid).await.status(), 200, "重放 hello 本身不报错");
    assert_eq!(c.post_raw("/api/rpc", ctr, body).await.0, 403, "但计数器没有被重置，旧请求仍不能重放");
}

#[tokio::test]
async fn evicted_session_id_can_never_come_back() {
    let s = start_with(Ipv4Addr::LOCALHOST, |c| c.max_sessions = 2).await;
    let first = random_cid();
    assert_eq!(hello(&s.base, &s.key(), &first).await.status(), 200);
    for _ in 0..2 {
        assert_eq!(hello(&s.base, &s.key(), &random_cid()).await.status(), 200);
    }
    assert_eq!(hello(&s.base, &s.key(), &first).await.status(), 409, "被淘汰的 cid 退役，不能复活");
}

// ---------- 加密信道 ----------

#[tokio::test]
async fn responses_are_ciphertext() {
    let s = start().await;
    let mut c = pair_qr(&s).await;
    let (ctr, body) = c.seal("/api/rpc", br#"{"op":"info"}"#);
    let (status, raw) = c.post_raw("/api/rpc", ctr, body).await;
    assert_eq!(status, 200);
    assert!(serde_json::from_slice::<serde_json::Value>(&raw).is_err(), "响应体不能是明文 JSON");
    let text = String::from_utf8_lossy(&raw);
    assert!(!text.contains("server_id") && !text.contains(&s.state.pin()));
}

#[tokio::test]
async fn unknown_session_is_401() {
    let s = start().await;
    let mut c = Client::from_secret(&s.base, random_cid(), &[1u8; 32]);
    assert_eq!(c.send("/api/rpc", br#"{"op":"info"}"#).await.unwrap_err(), 401);
}

#[tokio::test]
async fn tampered_replayed_or_moved_requests_are_403() {
    let s = start().await;
    let mut c = pair_qr(&s).await;

    let (ctr, mut body) = c.seal("/api/rpc", br#"{"op":"texts"}"#);
    body[0] ^= 1;
    assert_eq!(c.post_raw("/api/rpc", ctr, body).await.0, 403, "篡改");

    let (ctr, body) = c.seal("/api/rpc", br#"{"op":"texts"}"#);
    assert_eq!(c.post_raw("/api/rpc", ctr, body.clone()).await.0, 200);
    assert_eq!(c.post_raw("/api/rpc", ctr, body).await.0, 403, "重放");

    let (ctr, body) = c.seal("/api/rpc", br#"{"op":"texts"}"#);
    assert_eq!(c.post_raw("/api/down/abc/0", ctr, body).await.0, 403, "挪到别的接口");

    let (ctr, body) = c.seal("/api/rpc", br#"{"op":"texts"}"#);
    assert_eq!(c.post_raw("/api/rpc", ctr + 1, body).await.0, 403, "计数器对不上");
    assert!(c.rpc(json!({ "op": "texts" })).await["ok"].as_bool().unwrap(), "失败的请求不影响后续正常请求");
}

#[tokio::test]
async fn failed_decryption_does_not_burn_the_counter() {
    let s = start().await;
    let mut c = pair_qr(&s).await;
    let (ctr, body) = c.seal("/api/rpc", br#"{"op":"texts"}"#);
    let mut forged = body.clone();
    forged[5] ^= 0x40;
    assert_eq!(c.post_raw("/api/rpc", ctr, forged).await.0, 403);
    assert_eq!(c.post_raw("/api/rpc", ctr, body).await.0, 200, "伪造请求不能把合法计数器占掉");
}

#[tokio::test]
async fn bad_headers_and_content_type() {
    let s = start().await;
    let c = pair_qr(&s).await;
    let url = format!("{}/api/rpc", s.base);
    let resp = http().post(&url).header("X-LS-Sid", hex::encode(c.cid)).header("X-LS-Ctr", "1")
        .header("Content-Type", "text/plain").body("x").send().await.unwrap();
    assert_eq!(resp.status(), 415, "跨站简单请求只能发 text/plain，直接拒绝");
    let resp = http().post(&url).header("X-LS-Ctr", "1")
        .header("Content-Type", "application/octet-stream").body("x").send().await.unwrap();
    assert_eq!(resp.status(), 400, "缺会话头");
    let resp = http().post(&url).header("X-LS-Sid", hex::encode(c.cid)).header("X-LS-Ctr", "abc")
        .header("Content-Type", "application/octet-stream").body("x").send().await.unwrap();
    assert_eq!(resp.status(), 400, "计数器不是数字");
}

#[tokio::test]
async fn oversized_rpc_is_413() {
    // 只发请求头、不发正文：服务端凭声明的长度就要拒绝。
    // 不能让客户端真的发一个超长正文：服务端回完 413 就关连接，客户端这时还在写正文，
    // Windows 上连接会被重置，客户端偶尔读不到 413，测试就时好时坏。
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let s = start().await;
    let c = pair_qr(&s).await;
    let head = format!(
        "POST /api/rpc HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Type: application/octet-stream\r\n\
         X-LS-Sid: {}\r\nX-LS-Ctr: 1\r\nContent-Length: {}\r\n\r\n",
        s.port,
        hex::encode(c.cid),
        proto::MAX_RPC + proto::TAG_LEN + 1
    );
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", s.port)).await.unwrap();
    stream.write_all(head.as_bytes()).await.unwrap();
    let mut buf = vec![0u8; 256];
    let n = tokio::time::timeout(std::time::Duration::from_secs(3), stream.read(&mut buf)).await.unwrap().unwrap();
    assert!(String::from_utf8_lossy(&buf[..n]).starts_with("HTTP/1.1 413 "), "声明的长度超过上限，不读正文直接 413");
}

// ---------- 口令配对（SPAKE2） ----------

#[tokio::test]
async fn pin_pairing_works() {
    let s = start().await;
    let mut c = pair_pin(&s.base, &s.state.pin(), &server_id(&s).await).await.unwrap();
    assert_eq!(c.rpc(json!({ "op": "info" })).await["ok"], true);
}

#[tokio::test]
async fn wrong_pin_is_detected_and_cannot_finish() {
    let s = start().await;
    let wrong = if s.state.pin() == "000000" { "111111" } else { "000000" };
    assert_eq!(pair_pin(&s.base, wrong, &server_id(&s).await).await.err().unwrap(), "wrong_pin");
}

#[tokio::test]
async fn finishing_with_a_forged_confirmation_is_403() {
    let s = start().await;
    let cid = random_cid();
    let (_, msg) = proto::pake_start("123456", s.state.server_id(), rand_core::OsRng);
    let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &msg);
    let resp = http().post(format!("{}/api/pake/start", s.base))
        .json(&json!({ "cid": hex::encode(cid), "msg": b64 })).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let resp = http().post(format!("{}/api/pake/finish", s.base))
        .json(&json!({ "cid": hex::encode(cid), "confirm": hex::encode([0u8; 32]) })).send().await.unwrap();
    assert_eq!(resp.status(), 403);
}

#[tokio::test]
async fn pin_attempts_are_rate_limited_per_ip() {
    let s = start().await;
    let sid = server_id(&s).await;
    for _ in 0..5 {
        let _ = pair_pin(&s.base, "000001", &sid).await;
    }
    assert_eq!(pair_pin(&s.base, &s.state.pin(), &sid).await.err().unwrap(), "429");
}

#[tokio::test]
async fn successful_pairing_does_not_count_as_failure() {
    let s = start_with(Ipv4Addr::LOCALHOST, |c| c.per_ip_limit = 1000).await;
    let sid = server_id(&s).await;
    let pin = s.state.pin();
    for _ in 0..12 {
        pair_pin(&s.base, &pin, &sid).await.unwrap();
    }
    assert_eq!(s.state.pin(), pin);
}

// ---------- RPC ----------

#[tokio::test]
async fn texts_roundtrip_and_validation() {
    let s = start().await;
    let mut c = pair_qr(&s).await;
    let r = c.rpc(json!({ "op": "text_add", "text": "你好 <img src=x onerror=alert(1)>" })).await;
    assert_eq!(r["ok"], true);
    let list = c.rpc(json!({ "op": "texts" })).await;
    assert_eq!(list["texts"][0]["text"], "你好 <img src=x onerror=alert(1)>");
    for bad in [json!(""), json!("   "), json!("x".repeat(10_001)), json!(5)] {
        let r = c.rpc(json!({ "op": "text_add", "text": bad })).await;
        assert_eq!(r["ok"], false);
    }
}

#[tokio::test]
async fn list_uses_opaque_ids() {
    let s = start().await;
    std::fs::create_dir_all(s.share()).unwrap();
    std::fs::write(s.share().join("报告.pdf"), b"pdf").unwrap();
    std::fs::write(s.share().join(".lanshare-x.part"), b"tmp").unwrap();
    let mut c = pair_qr(&s).await;
    let list = c.rpc(json!({ "op": "list" })).await;
    let files = list["files"].as_array().unwrap();
    assert_eq!(files.len(), 1, "临时文件不出现在列表里");
    assert_eq!(files[0]["name"], "报告.pdf");
    assert_eq!(files[0]["size"], 3);
    let id = files[0]["id"].as_str().unwrap();
    assert_eq!(id.len(), 16);
    assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    let again = c.rpc(json!({ "op": "list" })).await;
    assert_eq!(again["files"][0]["id"], id, "同一次运行内 id 稳定");
}

#[tokio::test]
async fn unknown_op_is_an_error_not_a_crash() {
    let s = start().await;
    let mut c = pair_qr(&s).await;
    assert_eq!(c.rpc(json!({ "op": "rm -rf" })).await["ok"], false);
    let r = c.send("/api/rpc", b"not json").await.unwrap();
    assert_eq!(serde_json::from_slice::<serde_json::Value>(&r).unwrap()["ok"], false);
}

#[tokio::test]
async fn desktop_actions_work_locally() {
    let s = start().await;
    std::fs::create_dir_all(s.share()).unwrap();
    std::fs::write(s.share().join("a.txt"), b"1").unwrap();
    let mut c = pair_qr(&s).await;
    let id = c.rpc(json!({ "op": "list" })).await["files"][0]["id"].as_str().unwrap().to_string();
    assert_eq!(c.rpc(json!({ "op": "reveal", "id": id })).await["ok"], true);
    assert_eq!(c.rpc(json!({ "op": "open_folder" })).await["ok"], true);
    assert_eq!(c.rpc(json!({ "op": "reveal", "id": "0000000000000000" })).await["ok"], false);
    let calls = s.desktop.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].0, "reveal");
    assert!(calls[0].1.ends_with("a.txt"));
    assert_eq!(calls[1].0, "folder");
}

#[tokio::test]
async fn desktop_actions_refused_from_the_lan() {
    let Some(lan_ip) = lanshare::netinfo::lan_ips().into_iter().next() else {
        eprintln!("本机没有局域网地址，跳过");
        return;
    };
    let s = start_with(Ipv4Addr::UNSPECIFIED, |_| {}).await;
    std::fs::create_dir_all(s.share()).unwrap();
    std::fs::write(s.share().join("a.txt"), b"1").unwrap();
    let base = format!("http://{lan_ip}:{}", s.port);
    let cid = random_cid();
    assert_eq!(hello(&base, &s.key(), &cid).await.status(), 200);
    let mut c = Client::from_secret(&base, cid, &proto::session_secret_from_key(&s.key(), &cid));
    let info = c.rpc(json!({ "op": "info" })).await;
    assert_eq!(info["local"], false);
    assert!(info.get("folder").is_none(), "本机路径不告诉别的设备");
    let id = c.rpc(json!({ "op": "list" })).await["files"][0]["id"].as_str().unwrap().to_string();
    assert_eq!(c.rpc(json!({ "op": "reveal", "id": id })).await["error"], "only_local");
    assert_eq!(c.rpc(json!({ "op": "open_folder" })).await["error"], "only_local");
    assert!(s.desktop.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn logs_never_contain_secrets() {
    let s = start().await;
    let mut c = pair_qr(&s).await;
    c.rpc(json!({ "op": "text_add", "text": "机密文字" })).await;
    let _ = pair_pin(&s.base, &s.state.pin(), &server_id(&s).await).await;
    let _ = hello(&s.base, &[9u8; 32], &random_cid()).await;
    let logs = s.logs();
    for secret in [hex::encode(s.key()), s.state.pin(), "机密文字".to_string()] {
        assert!(!logs.contains(&secret), "日志里出现了秘密：{secret}\n{logs}");
    }
}
