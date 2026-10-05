//! 加密信道：所有登录后的请求和响应都是 ChaCha20-Poly1305 密文。
//!
//! 请求头 `X-LS-Sid`（会话 id）+ `X-LS-Ctr`（计数器）；aad 绑定请求路径；
//! 解密成功后才登记计数器，并且“检查 + 登记”在同一次加锁里完成，并发重放也只有一个能通过。

use std::net::SocketAddr;
use std::time::Instant;

use axum::extract::{ConnectInfo, Request};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use lanshare_proto::{self as proto, CHUNK, ChannelKeys, MAX_RPC, TAG_LEN};
use serde_json::json;

use super::approval::{Gate, device_label};
use super::{St, rpc, transfer};

pub(crate) struct Opened {
    pub(crate) cid: [u8; 16],
    pub(crate) ctr: u64,
    pub(crate) plain: Vec<u8>,
    s2c: [u8; 32],
}

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

/// 只看请求头就能做的检查，**在读正文之前**完成：类型、长度、会话存在、计数器在窗口内。
/// 这样不知道会话的人发来的请求，正文一个字节都不会被读进内存（独立安全审查 I-3）。
fn precheck(st: &super::AppState, headers: &HeaderMap, path: &str) -> Result<([u8; 16], u64, ChannelKeys), StatusCode> {
    if header_str(headers, "content-type") != Some("application/octet-stream") {
        return Err(StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }
    let declared = header_str(headers, "content-length").and_then(|s| s.parse::<usize>().ok());
    if declared.is_some_and(|n| n > body_limit(path)) {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }
    let cid: Option<[u8; 16]> =
        header_str(headers, "x-ls-sid").and_then(|s| hex::decode(s).ok()).and_then(|v| v.try_into().ok());
    let ctr = header_str(headers, "x-ls-ctr").and_then(|s| s.parse::<u64>().ok());
    let (Some(cid), Some(ctr)) = (cid, ctr) else {
        return Err(StatusCode::BAD_REQUEST);
    };
    // 第一次加锁：取钥匙、预检计数器（不登记）。解密在锁外做，不拖慢其他设备。
    let mut sessions = st.sessions.lock().expect("锁不会中毒");
    let Some(session) = sessions.get(&cid) else {
        return Err(StatusCode::UNAUTHORIZED);
    };
    if !session.window.check(ctr) {
        return Err(StatusCode::FORBIDDEN);
    }
    Ok((cid, ctr, session.keys.clone()))
}

/// 排队等全局正文名额最多等多久。
const QUEUE_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

/// 一个 IP 正在读的正文名额；drop 时退还。
struct BodySlot<'a> {
    st: &'a super::AppState,
    ip: std::net::IpAddr,
}

impl<'a> BodySlot<'a> {
    fn take(st: &'a super::AppState, ip: std::net::IpAddr) -> Option<Self> {
        let mut counts = st.bodies_per_ip.lock().expect("锁不会中毒");
        let n = counts.entry(ip).or_default();
        if *n >= st.max_bodies_per_ip {
            return None;
        }
        *n += 1;
        Some(BodySlot { st, ip })
    }
}

impl Drop for BodySlot<'_> {
    fn drop(&mut self) {
        let mut counts = self.st.bodies_per_ip.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(n) = counts.get_mut(&self.ip) {
            *n -= 1;
            if *n == 0 {
                counts.remove(&self.ip);
            }
        }
    }
}

fn body_limit(path: &str) -> usize {
    (if path == "/api/rpc" { MAX_RPC } else { CHUNK }) + TAG_LEN
}

/// 解密正文并登记计数器。
fn open_body(st: &super::AppState, cid: [u8; 16], ctr: u64, keys: &ChannelKeys, path: &str, body: &[u8]) -> Result<Opened, StatusCode> {
    let plain = proto::open(&keys.c2s, ctr, &proto::req_aad(path), body).map_err(|_| StatusCode::FORBIDDEN)?;
    // 第二次加锁：再查一次再登记，两个相同的重放请求并发到达时只有一个能通过。
    let mut sessions = st.sessions.lock().expect("锁不会中毒");
    let Some(session) = sessions.get(&cid) else {
        return Err(StatusCode::UNAUTHORIZED);
    };
    if !session.window.check(ctr) {
        return Err(StatusCode::FORBIDDEN);
    }
    session.window.commit(ctr);
    Ok(Opened { cid, ctr, plain, s2c: keys.s2c })
}

/// 新设备确认这道门：放行返回 None；不放行返回给 RPC 的回复（等确认 / 被拒绝 / 等的设备太多）。
fn gate(st: &super::AppState, ip: std::net::IpAddr, headers: &HeaderMap) -> Option<Vec<u8>> {
    let device = device_label(header_str(headers, "user-agent").unwrap_or(""));
    let now = Instant::now();
    let (gate, pop) = {
        let mut approvals = st.approvals.lock().expect("锁不会中毒");
        let gate = approvals.gate(ip, device, now);
        let pop = matches!(gate, Gate::Wait { new: true, .. }) && approvals.should_pop(now);
        (gate, pop)
    };
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
    if pop && let Some(desktop) = &st.desktop {
        desktop.open_app(&st.local_entry_url());
    }
    Some(rpc::reply(reply))
}

fn sealed_response(opened: &Opened, path: &str, payload: &[u8]) -> Response {
    let body = proto::seal(&opened.s2c, opened.ctr, &proto::resp_aad(path), payload);
    let mut response = body.into_response();
    response.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("application/octet-stream"));
    response
}

/// `/api/rpc`、`/api/up/..`、`/api/down/..` 共用的入口。
pub(crate) async fn sealed(state: St, ConnectInfo(peer): ConnectInfo<SocketAddr>, request: Request) -> Response {
    let st = state.0;
    let (parts, body) = request.into_parts();
    let path = parts.uri.path().to_string();
    let (cid, ctr, keys) = match precheck(&st, &parts.headers, &path) {
        Ok(checked) => checked,
        Err(status) => return status.into_response(),
    };
    // 同时在读的正文数有上限（每个约 1 MiB）：先占本 IP 的名额（满了立刻 503，复验 N-2），
    // 再排全局的队（最多等 30 秒）。内存占用有界，一个来源也占不光全局名额
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
    let opened = match open_body(&st, cid, ctr, &keys, &path, &body) {
        Ok(opened) => opened,
        Err(status) => return status.into_response(),
    };
    let local = peer.ip().is_loopback();
    // 局域网里的新设备：电脑上点了“允许”才放行。分块上传下载直接 403：没放行的设备还拿不到文件列表和上传号
    if !local && st.approve_new_devices && let Some(reply) = gate(&st, peer.ip(), &parts.headers) {
        return if path == "/api/rpc" { sealed_response(&opened, &path, &reply) } else { StatusCode::FORBIDDEN.into_response() };
    }
    let segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    let payload: Vec<u8> = match segments.as_slice() {
        ["api", "rpc"] => rpc::handle(&st, &opened.plain, local, opened.cid, peer.ip()).await,
        ["api", "up", upload, index] => {
            transfer::put_chunk(&st, upload, index, opened.plain.clone(), opened.cid).await
        }
        ["api", "down", file, index] => transfer::get_chunk(&st, file, index, &opened.plain, peer.ip()).await,
        _ => rpc::reply_error("not_found"),
    };
    sealed_response(&opened, &path, &payload)
}
