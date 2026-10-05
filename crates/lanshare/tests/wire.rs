//! 抓包测试：一个记录全部字节的 TCP 中转代理夹在客户端和服务端之间，
//! 完整走一遍“扫码配对 + 口令配对 + 上传 + 下载 + 文字”，然后断言线路上找不到任何秘密。
//! 这就是同一 WiFi 下抓包者能看到的全部内容。

mod common;

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use base64::Engine;
use common::*;
use lanshare_proto as proto;
use rand_core::{OsRng, RngCore};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// 启动一个转发到 `upstream` 的代理，返回 (代理地址, 录下的全部字节)。
async fn recording_proxy(upstream: SocketAddr) -> (SocketAddr, Arc<Mutex<Vec<u8>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let tape = Arc::new(Mutex::new(Vec::new()));
    let tape2 = tape.clone();
    tokio::spawn(async move {
        loop {
            let Ok((client, _)) = listener.accept().await else { return };
            let server = TcpStream::connect(upstream).await.unwrap();
            let (cr, cw) = client.into_split();
            let (sr, sw) = server.into_split();
            tokio::spawn(pump(cr, sw, tape2.clone()));
            tokio::spawn(pump(sr, cw, tape2.clone()));
        }
    });
    (addr, tape)
}

async fn pump(
    mut from: tokio::net::tcp::OwnedReadHalf,
    mut to: tokio::net::tcp::OwnedWriteHalf,
    tape: Arc<Mutex<Vec<u8>>>,
) {
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        match from.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                tape.lock().unwrap().extend_from_slice(&buf[..n]);
                if to.write_all(&buf[..n]).await.is_err() {
                    break;
                }
            }
        }
    }
    let _ = to.shutdown().await;
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
}

#[tokio::test]
async fn eavesdropper_sees_no_secrets() {
    let s = start().await;
    let (proxy, tape) = recording_proxy(SocketAddr::from(([127, 0, 0, 1], s.port))).await;
    let base = format!("http://{proxy}");

    // 扫码配对（经过代理）
    let cid = random_cid();
    assert_eq!(hello(&base, &s.key(), &cid).await.status(), 200);
    let qr_secret = proto::session_secret_from_key(&s.key(), &cid);
    let mut phone = Client::from_secret(&base, cid, &qr_secret);

    // 口令配对（经过代理）
    let mut laptop = pair_pin(&base, &s.state.pin(), s.state.server_id()).await.unwrap();

    // 上传一个文件、下载回来、发一条文字、拉一次信息（信息里含口令和二维码）
    let mut content = vec![0u8; 3 * proto::CHUNK + 777];
    OsRng.fill_bytes(&mut content);
    let file_name = "机密合同-2026终版.pdf";
    let text = "WiFi 密码是 hunter2-局域网快传";
    assert_eq!(phone.upload(file_name, &content).await.unwrap(), file_name);
    let f = laptop.find_file(file_name).await;
    let got = laptop.download(f["id"].as_str().unwrap(), content.len() as u64, f["mtime"].as_f64().unwrap()).await.unwrap();
    assert_eq!(got, content);
    assert_eq!(laptop.rpc(json!({ "op": "text_add", "text": text })).await["ok"], true);
    assert_eq!(phone.rpc(json!({ "op": "texts" })).await["texts"][0]["text"], text);
    let info = phone.rpc(json!({ "op": "info" })).await;
    assert!(info["qr_svg"].as_str().unwrap().contains("<svg"));

    let tape = tape.lock().unwrap().clone();
    assert!(tape.len() > content.len() * 2, "确认代理确实录到了上传和下载的流量（{} 字节）", tape.len());

    let key = s.key();
    let pin = s.state.pin();
    let b64url = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(key);
    let secrets: Vec<(&str, Vec<u8>)> = vec![
        ("二维码密钥 K（原始字节）", key.to_vec()),
        ("二维码密钥 K（hex）", hex::encode(key).into_bytes()),
        ("二维码密钥 K（base64url，二维码里的写法）", b64url.into_bytes()),
        ("口令", pin.into_bytes()),
        ("扫码会话秘密", qr_secret.to_vec()),
        ("扫码会话 c2s 密钥", phone.keys.c2s.to_vec()),
        ("口令会话 s2c 密钥", laptop.keys.s2c.to_vec()),
        ("文件名", file_name.as_bytes().to_vec()),
        ("文字内容", text.as_bytes().to_vec()),
        ("文件开头 64 字节", content[..64].to_vec()),
        ("文件中间 64 字节", content[content.len() / 2..content.len() / 2 + 64].to_vec()),
        ("文件结尾 64 字节", content[content.len() - 64..].to_vec()),
        ("svg 标签", b"<svg".to_vec()),
    ];
    for (what, secret) in &secrets {
        assert!(!contains(&tape, secret), "抓包里出现了：{what}");
    }
    // 反向自检：确认这个检查本身是有效的——明文协议字段确实能在录音里找到
    assert!(contains(&tape, b"/api/hello"), "录音里应该能看到明文的请求行");
    assert!(contains(&tape, hex::encode(cid).as_bytes()), "会话 id 本来就是明文，应该能找到");
}
