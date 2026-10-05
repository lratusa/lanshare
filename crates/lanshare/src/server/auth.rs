//! 配对：二维码握手（hello）、口令配对（SPAKE2）、会话表、限流，以及口令登录的累计失败暂停。

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

use axum::Json;
use axum::body::Bytes;
use axum::extract::{ConnectInfo, Request};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use lanshare_proto::{self as proto, ChannelKeys, ReplayWindow};
use rand_core::OsRng;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::json;

use super::St;

const WINDOW: Duration = Duration::from_secs(60);
const PAKE_TTL: Duration = Duration::from_secs(60);
const MAX_PENDING_PAKE: usize = 64;

pub(crate) struct Session {
    pub(crate) keys: ChannelKeys,
    pub(crate) window: ReplayWindow,
    last_used: Instant,
}

#[derive(Default)]
pub(crate) struct Sessions {
    live: HashMap<[u8; 16], Session>,
    /// 被淘汰或配对失败过的 cid：本次运行内永不再接受，防止重放旧握手“复活”会话、计数器归零。
    retired: HashSet<[u8; 16]>,
    pending: HashMap<[u8; 16], ([u8; 32], Instant)>,
}

pub(crate) enum Admit {
    Created,
    AlreadyLive,
    Refused,
}

impl Sessions {
    fn known(&self, cid: &[u8; 16]) -> bool {
        self.live.contains_key(cid) || self.retired.contains(cid) || self.pending.contains_key(cid)
    }

    fn admit(&mut self, cid: [u8; 16], secret: &[u8; 32], max: usize) -> Admit {
        if self.live.contains_key(&cid) {
            return Admit::AlreadyLive;
        }
        if self.retired.contains(&cid) || self.pending.contains_key(&cid) {
            return Admit::Refused;
        }
        if self.live.len() >= max
            && let Some(oldest) = self.live.iter().min_by_key(|(_, s)| s.last_used).map(|(k, _)| *k)
        {
            self.live.remove(&oldest);
            self.retired.insert(oldest);
        }
        self.live.insert(
            cid,
            Session { keys: ChannelKeys::derive(secret), window: ReplayWindow::new(), last_used: Instant::now() },
        );
        Admit::Created
    }

    fn is_live(&self, cid: &[u8; 16]) -> bool {
        self.live.contains_key(cid)
    }

    pub(crate) fn clear_pending(&mut self) {
        self.pending.clear();
    }

    pub(crate) fn get(&mut self, cid: &[u8; 16]) -> Option<&mut Session> {
        let session = self.live.get_mut(cid)?;
        session.last_used = Instant::now();
        Some(session)
    }
}

/// 限流：每 IP 的握手失败、每 IP 的口令尝试、全局口令尝试；以及未完成口令尝试的累计数。
///
/// 只限速挡不住长期猜口令：每分钟 20 次，一个月就有约一半概率猜中 6 位口令（独立安全审查 I-1）。
/// 所以累计未完成的尝试到上限就**暂停**口令登录，只能在本机主界面上重新开启（同时换新口令）。
/// 每次开启期间被猜中的概率不超过 上限 / 1,000,000。扫码登录不受影响。
#[derive(Default)]
pub(crate) struct Guard {
    hello_failures: HashMap<IpAddr, VecDeque<Instant>>,
    pake_attempts: HashMap<IpAddr, VecDeque<Instant>>,
    pake_global: VecDeque<Instant>,
    outstanding: usize,
    paused: bool,
    /// 握手成功、开了新会话的时刻：每个 IP 一份，再加全局一份。
    new_sessions: HashMap<IpAddr, VecDeque<Instant>>,
    new_sessions_global: VecDeque<Instant>,
}

impl Guard {
    pub(crate) fn pin_paused(&self) -> bool {
        self.paused
    }

    pub(crate) fn resume(&mut self) {
        self.paused = false;
        self.outstanding = 0;
    }

    /// 记一次“开新会话”。这 60 秒里本 IP 或全局开得太多，就不记，返回 false。
    fn take_new_session(&mut self, ip: IpAddr, now: Instant, per_ip: usize, global: usize) -> bool {
        let mine = recent(self.new_sessions.entry(ip).or_default(), now);
        if mine >= per_ip || recent(&mut self.new_sessions_global, now) >= global {
            return false;
        }
        self.new_sessions.entry(ip).or_default().push_back(now);
        self.new_sessions_global.push_back(now);
        true
    }
}

fn recent(q: &mut VecDeque<Instant>, now: Instant) -> usize {
    while q.front().is_some_and(|t| now.duration_since(*t) > WINDOW) {
        q.pop_front();
    }
    q.len()
}

fn parse_cid(s: &str) -> Option<[u8; 16]> {
    hex::decode(s).ok()?.try_into().ok()
}

/// 请求体解析成 JSON；格式不对、字段缺失一律当作 400，不暴露解析细节。
fn parse<T: DeserializeOwned>(body: &Bytes) -> Option<T> {
    serde_json::from_slice(body).ok()
}

fn error(status: StatusCode, code: &str) -> Response {
    (status, Json(json!({ "ok": false, "error": code }))).into_response()
}

/// 握手接口只收 `application/json`：浏览器跨站发这种类型必须先预检，而服务端不回应预检，
/// 别的网站就没法借用户的浏览器发口令尝试（审查 M-2）。`text/plain` 之类的“简单请求”不需要预检。
/// 握手请求体的上限：这几个接口的 JSON 都只有几百字节。
const HANDSHAKE_MAX: usize = 4096;

/// 读握手接口的小正文：限量、限时（第三轮复验 R3-1：不限时的话，一个连接每隔一会儿发一个字节能挂几十个小时）。
async fn read_small_body(st: &super::AppState, request: Request) -> Result<(HeaderMap, Bytes), Response> {
    let (parts, body) = request.into_parts();
    if !is_json(&parts.headers) {
        return Err(error(StatusCode::UNSUPPORTED_MEDIA_TYPE, "json_only"));
    }
    match tokio::time::timeout(st.handshake_timeout, axum::body::to_bytes(body, HANDSHAKE_MAX)).await {
        Ok(Ok(bytes)) => Ok((parts.headers, bytes)),
        Ok(Err(_)) => Err(error(StatusCode::PAYLOAD_TOO_LARGE, "too_large")),
        Err(_) => Err(error(StatusCode::REQUEST_TIMEOUT, "timeout")),
    }
}

fn is_json(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(';').next().unwrap_or("").trim().eq_ignore_ascii_case("application/json"))
}

#[derive(Deserialize)]
pub(crate) struct HelloBody {
    cid: String,
    tag: String,
}

/// 扫码配对：客户端出示 HMAC(K, cid) 证明自己从二维码里拿到了密钥。
pub(crate) async fn hello(state: St, ConnectInfo(peer): ConnectInfo<SocketAddr>, request: Request) -> Response {
    let st = state.0;
    let body = match read_small_body(&st, request).await {
        Ok((_, body)) => body,
        Err(response) => return response,
    };
    let ip = peer.ip();
    let now = Instant::now();
    {
        let mut guard = st.guard.lock().expect("锁不会中毒");
        let failures = guard.hello_failures.entry(ip).or_default();
        if recent(failures, now) >= st.per_ip_limit {
            return error(StatusCode::TOO_MANY_REQUESTS, "too_many_attempts");
        }
    }
    let Some(body) = parse::<HelloBody>(&body) else { return error(StatusCode::BAD_REQUEST, "bad_request") };
    let (Some(cid), Ok(tag)) = (parse_cid(&body.cid), hex::decode(&body.tag)) else {
        return error(StatusCode::BAD_REQUEST, "bad_request");
    };
    if !proto::verify_hello(&st.key(), &cid, &tag) {
        st.guard.lock().expect("锁不会中毒").hello_failures.entry(ip).or_default().push_back(now);
        st.log(&format!("扫码配对失败：来自 {ip} 的握手标签不对"));
        return error(StatusCode::FORBIDDEN, "bad_tag");
    }
    // 拿到 K 的人也不能无限地开新会话：会话表满了，每开一个就挤掉一个旧的，
    // 旧会话号要记进 retired，这次运行内一直占着内存（同一个会话号再握手不算新开）
    let live = st.sessions.lock().expect("锁不会中毒").is_live(&cid);
    if !live && !st.guard.lock().expect("锁不会中毒").take_new_session(ip, now, st.new_sessions_per_ip, st.new_sessions_global) {
        return error(StatusCode::TOO_MANY_REQUESTS, "too_many_sessions");
    }
    let secret = proto::session_secret_from_key(&st.key(), &cid);
    let admitted = st.sessions.lock().expect("锁不会中毒").admit(cid, &secret, st.max_sessions);
    match admitted {
        Admit::Created => st.log(&format!("新设备已配对（扫码）：{ip}")),
        Admit::AlreadyLive => {}
        Admit::Refused => return error(StatusCode::CONFLICT, "cid_used"),
    }
    Json(json!({ "ok": true, "server_id": st.server_id() })).into_response()
}

#[derive(Deserialize)]
pub(crate) struct PakeStartBody {
    cid: String,
    msg: String,
}

/// 口令配对第一步：双方交换 SPAKE2 消息；服务端附上自己的确认标签。
pub(crate) async fn pake_start(state: St, ConnectInfo(peer): ConnectInfo<SocketAddr>, request: Request) -> Response {
    let st = state.0;
    let body = match read_small_body(&st, request).await {
        Ok((_, body)) => body,
        Err(response) => return response,
    };
    let ip = peer.ip();
    let now = Instant::now();
    let Some(body) = parse::<PakeStartBody>(&body) else { return error(StatusCode::BAD_REQUEST, "bad_request") };
    let (Some(cid), Ok(peer_msg)) =
        (parse_cid(&body.cid), base64::engine::general_purpose::STANDARD.decode(&body.msg))
    else {
        return error(StatusCode::BAD_REQUEST, "bad_request");
    };
    // 暂停检查、限速、“未完成尝试”计数在同一次加锁里完成，并且**先占位再做 SPAKE2 计算**：
    // 并发的请求不可能一起越过上限（复验 N-4：原来先算后计数，上限 30 实际能到 49）。
    // 计数到上限的这一次本身仍然受理（它是第 N 次），之后的一律 423。
    let pause_now = {
        let mut guard = st.guard.lock().expect("锁不会中毒");
        if guard.paused {
            return error(StatusCode::LOCKED, "pin_paused");
        }
        let per_ip = guard.pake_attempts.entry(ip).or_default();
        if recent(per_ip, now) >= st.per_ip_limit {
            return error(StatusCode::TOO_MANY_REQUESTS, "too_many_attempts");
        }
        if recent(&mut guard.pake_global, now) >= st.global_limit {
            return error(StatusCode::TOO_MANY_REQUESTS, "too_many_attempts");
        }
        guard.pake_attempts.entry(ip).or_default().push_back(now);
        guard.pake_global.push_back(now);
        // 计入“未完成的尝试”；配对成功时再减回去
        guard.outstanding += 1;
        if guard.outstanding >= st.pin_fail_limit {
            guard.paused = true;
            true
        } else {
            false
        }
    };

    let pin = st.pin();
    let (pake_state, my_msg) = proto::pake_start(&pin, st.server_id(), OsRng);
    let Ok(pake_key) = proto::pake_finish(pake_state, &peer_msg) else {
        return error(StatusCode::BAD_REQUEST, "bad_message");
    };
    {
        let mut sessions = st.sessions.lock().expect("锁不会中毒");
        sessions.pending.retain(|_, (_, at)| now.duration_since(*at) < PAKE_TTL);
        if sessions.known(&cid) {
            return error(StatusCode::CONFLICT, "cid_used");
        }
        if sessions.pending.len() >= MAX_PENDING_PAKE {
            return error(StatusCode::TOO_MANY_REQUESTS, "too_many_attempts");
        }
        sessions.pending.insert(cid, (pake_key, now));
    }
    if pause_now {
        st.sessions.lock().expect("锁不会中毒").clear_pending();
        st.log("口令尝试失败次数过多，已暂停口令登录（在电脑上的主界面里可以重新开启）");
    }
    let b64 = base64::engine::general_purpose::STANDARD;
    Json(json!({
        "ok": true,
        "msg": b64.encode(&my_msg),
        "confirm": hex::encode(proto::confirm_tag(&pake_key, "server", &cid)),
    }))
    .into_response()
}

#[derive(Deserialize)]
pub(crate) struct PakeFinishBody {
    cid: String,
    confirm: String,
}

/// 口令配对第二步：客户端出示确认标签，证明它算出了同一把钥匙（也就是输对了口令）。
pub(crate) async fn pake_finish(state: St, ConnectInfo(peer): ConnectInfo<SocketAddr>, request: Request) -> Response {
    let st = state.0;
    let body = match read_small_body(&st, request).await {
        Ok((_, body)) => body,
        Err(response) => return response,
    };
    if st.pin_paused() {
        // 暂停之后，即使口令猜对了也不能完成配对（复验 N-4）
        return error(StatusCode::LOCKED, "pin_paused");
    }
    let Some(body) = parse::<PakeFinishBody>(&body) else { return error(StatusCode::BAD_REQUEST, "bad_request") };
    let (Some(cid), Ok(tag)) = (parse_cid(&body.cid), hex::decode(&body.confirm)) else {
        return error(StatusCode::BAD_REQUEST, "bad_request");
    };
    let mut sessions = st.sessions.lock().expect("锁不会中毒");
    let Some((pake_key, at)) = sessions.pending.remove(&cid) else {
        return error(StatusCode::FORBIDDEN, "no_pending");
    };
    if at.elapsed() >= PAKE_TTL || !proto::verify_confirm(&pake_key, "client", &cid, &tag) {
        sessions.retired.insert(cid);
        drop(sessions);
        st.log(&format!("口令配对失败：来自 {}", peer.ip()));
        return error(StatusCode::FORBIDDEN, "bad_confirm");
    }
    let secret = proto::session_secret_from_pake(&pake_key, &cid);
    let admitted = sessions.admit(cid, &secret, st.max_sessions);
    drop(sessions);
    if !matches!(admitted, Admit::Created) {
        return error(StatusCode::CONFLICT, "cid_used");
    }
    {
        let mut guard = st.guard.lock().expect("锁不会中毒");
        guard.outstanding = guard.outstanding.saturating_sub(1);
    }
    st.log(&format!("新设备已配对（口令）：{}", peer.ip()));
    Json(json!({ "ok": true, "server_id": st.server_id() })).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_sessions_are_counted_per_minute() {
        let mut guard = Guard::default();
        let ip = IpAddr::from([192, 168, 1, 7]);
        let t = Instant::now();
        assert!(guard.take_new_session(ip, t, 2, 100));
        assert!(guard.take_new_session(ip, t, 2, 100));
        assert!(!guard.take_new_session(ip, t, 2, 100), "这一分钟里的第 3 个");
        assert!(guard.take_new_session(IpAddr::from([192, 168, 1, 8]), t, 2, 100), "别的 IP 不受影响");
        assert!(guard.take_new_session(ip, t + WINDOW + Duration::from_secs(1), 2, 100), "过了一分钟又可以");

        let mut guard = Guard::default();
        for last in 1..=3 {
            assert!(guard.take_new_session(IpAddr::from([192, 168, 1, last]), t, 100, 3));
        }
        assert!(!guard.take_new_session(IpAddr::from([192, 168, 1, 9]), t, 100, 3), "全局这一分钟已经开了 3 个");
    }
}
