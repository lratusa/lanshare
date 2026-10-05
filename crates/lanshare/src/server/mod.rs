//! HTTP 服务：配置、共享状态、路由与中间件。
//!
//! 协议见 `docs/superpowers/specs/2026-10-02-lanshare-v2-rust-design.md`。

mod approval;
mod assets;
mod auth;
mod channel;
mod conn;
mod rpc;
mod transfer;

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use base64::Engine;
use rand_core::{OsRng, RngCore};

use crate::storage::Storage;
use crate::textboard::TextBoard;

/// 只给本机主界面用的桌面操作（在资源管理器里定位文件、打开共享文件夹）。
pub trait Desktop: Send + Sync {
    fn reveal(&self, path: &Path);
    fn open_folder(&self, path: &Path);
    /// 有新设备等确认、主界面又没开着时，把主界面弹出来。
    fn open_app(&self, url: &str);
}

pub type Logger = Arc<dyn Fn(&str) + Send + Sync>;

/// 启动参数。限额字段有默认值，测试里可以调小/调大。
pub struct Config {
    pub share_dir: PathBuf,
    /// 不给就随机生成（正常运行时都不给）。
    pub key: Option<[u8; 32]>,
    pub pin: Option<String>,
    pub lan_ips: Vec<Ipv4Addr>,
    pub desktop: Option<Arc<dyn Desktop>>,
    pub log: Logger,
    pub max_sessions: usize,
    /// 同一 IP 每 60 秒最多几次口令尝试 / 握手失败。
    pub per_ip_limit: usize,
    /// 全局每 60 秒最多几次口令尝试。
    pub global_limit: usize,
    /// 每 60 秒最多开几个新会话（扫码握手成功）：每个 IP、全局。拿到 K 的人也不能无限地开。
    pub new_sessions_per_ip: usize,
    pub new_sessions_global: usize,
    /// 累计多少次没完成的口令尝试后暂停口令登录（要在本机主界面上重新开启）。
    pub pin_fail_limit: usize,
    /// 局域网里第一次连上来的设备，要在本机主界面上点“允许”才能用。
    pub approve_new_devices: bool,
    /// 同时进行中的上传：全局上限、每个会话的上限。
    pub max_uploads: usize,
    pub max_uploads_per_session: usize,
    /// 多久没有新块到达的上传视为放弃，删掉临时文件、释放名额。
    pub upload_idle: Duration,
    /// 开始上传时，预占空间后磁盘至少还要剩这么多。
    pub disk_reserve: u64,
    /// 读一个请求正文最多等多久。
    pub body_timeout: Duration,
    /// 连接上多久没有任何收发就断开。
    pub idle_timeout: Duration,
    /// 同时保持的连接数上限：局域网来的（全部 IP 合计）、本机回环来的（单独一池，局域网占满了本机界面也能用）、每个 IP。
    pub max_connections: usize,
    pub max_local_connections: usize,
    pub max_connections_per_ip: usize,
    /// 一个请求的请求头必须在这么长时间内收完。
    pub header_timeout: Duration,
    /// 握手接口（hello、PAKE）的小正文必须在这么长时间内收完。
    pub handshake_timeout: Duration,
    /// 同时在读的加密请求正文数上限（每个最多约 1 MiB，限住内存）：全局、每个 IP。
    pub max_inflight_bodies: usize,
    pub max_bodies_per_ip: usize,
}

impl Config {
    pub fn new(share_dir: impl Into<PathBuf>) -> Self {
        Config {
            share_dir: share_dir.into(),
            key: None,
            pin: None,
            lan_ips: Vec::new(),
            desktop: None,
            log: Arc::new(|_| {}),
            max_sessions: 256,
            per_ip_limit: 5,
            global_limit: 20,
            new_sessions_per_ip: 30,
            new_sessions_global: 300,
            pin_fail_limit: 30,
            approve_new_devices: true,
            max_uploads: 64,
            max_uploads_per_session: 4,
            upload_idle: Duration::from_secs(600),
            disk_reserve: 512 * 1024 * 1024,
            body_timeout: Duration::from_secs(300),
            idle_timeout: Duration::from_secs(60),
            max_connections: 256,
            max_local_connections: 32,
            max_connections_per_ip: 16,
            header_timeout: Duration::from_secs(10),
            handshake_timeout: Duration::from_secs(10),
            max_inflight_bodies: 32,
            max_bodies_per_ip: 8,
        }
    }
}

/// 一次运行中所有请求共享的状态。
pub struct AppState {
    key: [u8; 32],
    server_id: String,
    pin: Mutex<String>,
    port: u16,
    lan_ips: Vec<Ipv4Addr>,
    pub(crate) storage: Storage,
    pub(crate) texts: TextBoard,
    pub(crate) sessions: Mutex<auth::Sessions>,
    pub(crate) guard: Mutex<auth::Guard>,
    pub(crate) approvals: Mutex<approval::Approvals>,
    pub(crate) approve_new_devices: bool,
    pub(crate) files: Mutex<FileIds>,
    pub(crate) uploads: Mutex<transfer::Uploads>,
    pub(crate) desktop: Option<Arc<dyn Desktop>>,
    pub(crate) log: Logger,
    pub(crate) max_sessions: usize,
    pub(crate) per_ip_limit: usize,
    pub(crate) global_limit: usize,
    pub(crate) new_sessions_per_ip: usize,
    pub(crate) new_sessions_global: usize,
    pub(crate) pin_fail_limit: usize,
    pub(crate) max_uploads: usize,
    pub(crate) max_uploads_per_session: usize,
    pub(crate) upload_idle: Duration,
    pub(crate) disk_reserve: u64,
    pub(crate) body_timeout: Duration,
    pub(crate) idle_timeout: Duration,
    pub(crate) max_connections: usize,
    pub(crate) max_local_connections: usize,
    pub(crate) max_connections_per_ip: usize,
    pub(crate) header_timeout: Duration,
    pub(crate) handshake_timeout: Duration,
    pub(crate) bodies: Arc<tokio::sync::Semaphore>,
    pub(crate) max_bodies_per_ip: usize,
    pub(crate) bodies_per_ip: Mutex<HashMap<IpAddr, usize>>,
    /// 检查剩余空间 + 预占空间必须一个接一个做，否则并发的上传会看到同一个剩余空间（复验 N-5）。
    pub(crate) alloc_lock: Mutex<()>,
}

/// 文件名 ↔ 不透明 id。路径里只出现 id，文件名只在加密的列表里出现。
#[derive(Default)]
pub(crate) struct FileIds {
    by_name: HashMap<String, String>,
    by_id: HashMap<String, String>,
}

impl FileIds {
    pub(crate) fn id_for(&mut self, name: &str) -> String {
        if let Some(id) = self.by_name.get(name) {
            return id.clone();
        }
        let id = random_hex(8);
        self.by_name.insert(name.to_string(), id.clone());
        self.by_id.insert(id.clone(), name.to_string());
        id
    }

    pub(crate) fn name_for(&self, id: &str) -> Option<String> {
        self.by_id.get(id).cloned()
    }
}

pub(crate) fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    OsRng.fill_bytes(&mut buf);
    hex::encode(buf)
}

/// 均匀分布的 6 位数字口令（拒绝采样，避免取模偏差）。
pub(crate) fn random_pin() -> String {
    loop {
        let n = OsRng.next_u32();
        if n < u32::MAX - (u32::MAX % 1_000_000) {
            return format!("{:06}", n % 1_000_000);
        }
    }
}

impl AppState {
    pub fn new(config: Config, port: u16) -> Self {
        let key = config.key.unwrap_or_else(|| {
            let mut k = [0u8; 32];
            OsRng.fill_bytes(&mut k);
            k
        });
        AppState {
            key,
            server_id: random_hex(16),
            pin: Mutex::new(config.pin.unwrap_or_else(random_pin)),
            port,
            lan_ips: config.lan_ips,
            storage: Storage::new(config.share_dir),
            texts: TextBoard::new(),
            sessions: Mutex::new(auth::Sessions::default()),
            guard: Mutex::new(auth::Guard::default()),
            approvals: Mutex::new(approval::Approvals::default()),
            approve_new_devices: config.approve_new_devices,
            files: Mutex::new(FileIds::default()),
            uploads: Mutex::new(transfer::Uploads::default()),
            desktop: config.desktop,
            log: config.log,
            max_sessions: config.max_sessions,
            per_ip_limit: config.per_ip_limit,
            global_limit: config.global_limit,
            new_sessions_per_ip: config.new_sessions_per_ip,
            new_sessions_global: config.new_sessions_global,
            pin_fail_limit: config.pin_fail_limit,
            max_uploads: config.max_uploads,
            max_uploads_per_session: config.max_uploads_per_session,
            upload_idle: config.upload_idle,
            disk_reserve: config.disk_reserve,
            body_timeout: config.body_timeout,
            idle_timeout: config.idle_timeout,
            max_connections: config.max_connections,
            max_local_connections: config.max_local_connections,
            max_connections_per_ip: config.max_connections_per_ip,
            header_timeout: config.header_timeout,
            handshake_timeout: config.handshake_timeout,
            bodies: Arc::new(tokio::sync::Semaphore::new(config.max_inflight_bodies)),
            max_bodies_per_ip: config.max_bodies_per_ip,
            bodies_per_ip: Mutex::new(HashMap::new()),
            alloc_lock: Mutex::new(()),
        }
    }

    pub fn key(&self) -> [u8; 32] {
        self.key
    }

    pub fn server_id(&self) -> &str {
        &self.server_id
    }

    pub fn pin(&self) -> String {
        self.pin.lock().expect("口令锁不会中毒").clone()
    }

    /// 口令登录是否因为累计失败太多而暂停了。
    pub fn pin_paused(&self) -> bool {
        self.guard.lock().expect("锁不会中毒").pin_paused()
    }

    /// 本机主界面上“重新开启口令登录”：换一个新口令，失败计数清零。返回新口令。
    pub(crate) fn resume_pin(&self) -> String {
        let pin = random_pin();
        *self.pin.lock().expect("口令锁不会中毒") = pin.clone();
        self.guard.lock().expect("锁不会中毒").resume();
        self.sessions.lock().expect("锁不会中毒").clear_pending();
        pin
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn share_dir(&self) -> &Path {
        self.storage.root()
    }

    pub fn lan_ips(&self) -> &[Ipv4Addr] {
        &self.lan_ips
    }

    /// 给手机用的地址（不含密钥），第一个是主地址。
    pub fn base_urls(&self) -> Vec<String> {
        let hosts = if self.lan_ips.is_empty() { vec![Ipv4Addr::LOCALHOST] } else { self.lan_ips.clone() };
        hosts.iter().map(|ip| format!("http://{ip}:{}/", self.port)).collect()
    }

    /// 二维码密钥的 base64url 写法（写实例记录、info 文件用）。
    pub fn key_b64(&self) -> String {
        self.key_fragment()
    }

    /// 启动时清理上次异常退出留下的临时文件，返回删除个数。
    pub fn cleanup_stale_temp(&self) -> usize {
        self.storage.cleanup_stale_temp()
    }

    pub(crate) fn key_fragment(&self) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(self.key)
    }

    /// 二维码内容：主地址 + `#密钥`。`#` 后面的部分浏览器不会发到网络上。
    pub fn entry_url(&self) -> String {
        format!("{}#{}", self.base_urls()[0], self.key_fragment())
    }

    /// 本机主界面的地址。
    pub fn local_entry_url(&self) -> String {
        format!("http://127.0.0.1:{}/#{}", self.port, self.key_fragment())
    }

    pub(crate) fn log(&self, line: &str) {
        (self.log)(line);
    }
}

/// 只接受 IP 字面量或 localhost 作为 Host：DNS rebinding 必须借助域名，IP 字面量天然免疫。
async fn check_host(request: Request, next: Next) -> Response {
    let host = request.headers().get(header::HOST).and_then(|h| h.to_str().ok()).unwrap_or("");
    let name = match host.rsplit_once(':') {
        Some((name, port)) if !name.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => name,
        _ => host,
    };
    let name = name.trim_start_matches('[').trim_end_matches(']');
    if name.eq_ignore_ascii_case("localhost") || name.parse::<IpAddr>().is_ok() {
        next.run(request).await
    } else {
        (StatusCode::MISDIRECTED_REQUEST, "unknown host").into_response()
    }
}

async fn security_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let h = response.headers_mut();
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'none'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self'; img-src 'self' blob:; \
             connect-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
        ),
    );
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// 公开：本次运行的标识（不是秘密），页面用它判断本地存的密钥是否属于当前这次运行。
async fn server_id(State(st): St) -> Response {
    axum::Json(serde_json::json!({ "server_id": st.server_id(), "version": env!("CARGO_PKG_VERSION") })).into_response()
}

/// 带 Origin 头的请求（浏览器发的）必须来自本页面自己：挡住别的网站借用户的浏览器发请求（审查 M-2）。
/// 没有 Origin 的请求（非浏览器客户端、同源 GET）照常放行。
async fn check_origin(request: Request, next: Next) -> Response {
    if let Some(origin) = request.headers().get(header::ORIGIN) {
        let host = request.headers().get(header::HOST).and_then(|h| h.to_str().ok()).unwrap_or("");
        let origin_host = origin.to_str().ok().and_then(|o| o.strip_prefix("http://")).unwrap_or("");
        if host.is_empty() || !origin_host.eq_ignore_ascii_case(host) {
            return (StatusCode::FORBIDDEN, "cross-origin request refused").into_response();
        }
    }
    next.run(request).await
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(assets::index))
        .route("/app.js", get(assets::app_js))
        .route("/proto.js", get(assets::proto_js))
        .route("/style.css", get(assets::style_css))
        .route("/lanshare.wasm", get(assets::wasm))
        .route("/favicon.ico", get(assets::favicon))
        .route("/api/server", get(server_id))
        .route("/api/hello", post(auth::hello))
        .route("/api/pake/start", post(auth::pake_start))
        .route("/api/pake/finish", post(auth::pake_finish))
        // 加密接口自己控制正文读取：先验请求头，再在限量、限时的情况下读正文
        .route("/api/rpc", post(channel::sealed))
        .route("/api/up/{upload}/{index}", post(channel::sealed))
        .route("/api/down/{file}/{index}", post(channel::sealed))
        .layer(middleware::from_fn(check_origin))
        .layer(middleware::from_fn(security_headers))
        .layer(middleware::from_fn(check_host))
        .with_state(state)
}

/// 在已绑定的监听器上提供服务，直到出错。
pub async fn serve(listener: tokio::net::TcpListener, state: Arc<AppState>) -> std::io::Result<()> {
    serve_with_shutdown(listener, state, std::future::pending()).await
}

/// 同上，`shutdown` 完成时停止接受新连接，等进行中的请求结束（最多 3 秒；托盘“退出”时用）。
///
/// 没用 `axum::serve`：它不给 hyper 配计时器，读请求头没有时限（独立安全审查 I-3、复验 N-1）。
/// 这里自己接收连接：先过准入（每 IP 上限、本机单独一池），再交给 hyper，配上计时器和请求头时限。
pub async fn serve_with_shutdown(
    listener: tokio::net::TcpListener,
    state: Arc<AppState>,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    use axum::extract::ConnectInfo;
    use hyper_util::rt::{TokioIo, TokioTimer};
    use tower::ServiceExt;

    tokio::spawn(transfer::reap_forever(Arc::downgrade(&state)));
    let app = router(state.clone());
    let admission = conn::Admission::new(state.max_connections, state.max_local_connections, state.max_connections_per_ip);
    let graceful = hyper_util::server::graceful::GracefulShutdown::new();
    let mut shutdown = std::pin::pin!(shutdown);
    loop {
        let accepted = tokio::select! {
            accepted = listener.accept() => accepted,
            _ = &mut shutdown => break,
        };
        let (stream, peer) = match accepted {
            Ok(pair) => pair,
            // 对方刚连上就断了之类的连接级错误：直接接下一个（和 axum 自带的处理一样，第三轮复验 R3-3）
            Err(e) if is_connection_error(&e) => continue,
            // 句柄耗尽之类的错误：稍等再接；等的时候也响应“退出”
            Err(_) => tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(50)) => continue,
                _ = &mut shutdown => break,
            },
        };
        // 超过上限的连接直接关掉（stream 在这里被 drop），不排队
        let Some(ticket) = admission.admit(peer.ip()) else { continue };
        let _ = stream.set_nodelay(true);
        let io = TokioIo::new(conn::GuardedIo::new(stream, ticket, state.idle_timeout));
        let app = app.clone();
        let service = hyper::service::service_fn(move |mut request: hyper::Request<hyper::body::Incoming>| {
            // handler 里用 ConnectInfo<SocketAddr> 拿对端地址
            request.extensions_mut().insert(ConnectInfo(peer));
            app.clone().oneshot(request.map(axum::body::Body::new))
        });
        let mut builder = hyper::server::conn::http1::Builder::new();
        builder
            .timer(TokioTimer::new())
            .header_read_timeout(state.header_timeout)
            .max_buf_size(64 * 1024); // 本协议的请求头都很小；正文是流式读取的，不受这个缓冲限制
        let connection = graceful.watch(builder.serve_connection(io, service));
        tokio::spawn(async move {
            let _ = connection.await;
        });
    }
    tokio::select! {
        _ = graceful.shutdown() => {}
        _ = tokio::time::sleep(Duration::from_secs(3)) => {}
    }
    Ok(())
}

fn is_connection_error(e: &std::io::Error) -> bool {
    use std::io::ErrorKind::{ConnectionAborted, ConnectionRefused, ConnectionReset};
    matches!(e.kind(), ConnectionAborted | ConnectionRefused | ConnectionReset)
}

/// 供 handler 使用的 State 类型别名。
pub(crate) type St = State<Arc<AppState>>;
