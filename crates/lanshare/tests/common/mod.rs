//! 集成测试工具：在随机端口真实启动服务端，并提供一个按协议说话的测试客户端。
#![allow(dead_code)]

use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use lanshare::server::{self, AppState, Config, Desktop};
use lanshare_proto as proto;
use rand_core::{OsRng, RngCore};
use serde_json::{Value, json};

#[derive(Default)]
pub struct FakeDesktop {
    pub calls: Mutex<Vec<(String, PathBuf)>>,
}

impl Desktop for FakeDesktop {
    fn reveal(&self, path: &Path) {
        self.calls.lock().unwrap().push(("reveal".into(), path.to_path_buf()));
    }
    fn open_folder(&self, path: &Path) {
        self.calls.lock().unwrap().push(("folder".into(), path.to_path_buf()));
    }
    fn open_app(&self, url: &str) {
        self.calls.lock().unwrap().push(("app".into(), PathBuf::from(url)));
    }
}

pub struct TestServer {
    pub base: String,
    pub port: u16,
    pub state: Arc<AppState>,
    pub dir: tempfile::TempDir,
    pub desktop: Arc<FakeDesktop>,
    pub logs: Arc<Mutex<Vec<String>>>,
}

impl TestServer {
    pub fn share(&self) -> PathBuf {
        self.dir.path().join("share")
    }
    pub fn key(&self) -> [u8; 32] {
        self.state.key()
    }
    pub fn logs(&self) -> String {
        self.logs.lock().unwrap().join("\n")
    }
}

/// 在 127.0.0.1 的随机端口启动。`tweak` 可以改测试用的限额。
pub async fn start_with(bind: Ipv4Addr, tweak: impl FnOnce(&mut Config)) -> TestServer {
    let dir = tempfile::tempdir().unwrap();
    let desktop = Arc::new(FakeDesktop::default());
    let logs = Arc::new(Mutex::new(Vec::new()));
    let sink = logs.clone();
    let listener = tokio::net::TcpListener::bind(SocketAddr::from((bind, 0))).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let mut config = Config::new(dir.path().join("share"));
    config.lan_ips = lanshare::netinfo::lan_ips();
    config.desktop = Some(desktop.clone());
    config.log = Arc::new(move |line: &str| sink.lock().unwrap().push(line.to_string()));
    config.approve_new_devices = false; // 新设备确认只在 tests/approval.rs 里打开，别的测试各测各的
    tweak(&mut config);
    let state = Arc::new(AppState::new(config, port));
    tokio::spawn(server::serve(listener, state.clone()));
    TestServer { base: format!("http://127.0.0.1:{port}"), port, state, dir, desktop, logs }
}

pub async fn start() -> TestServer {
    start_with(Ipv4Addr::LOCALHOST, |_| {}).await
}

pub fn http() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}

pub fn random_cid() -> [u8; 16] {
    let mut cid = [0u8; 16];
    OsRng.fill_bytes(&mut cid);
    cid
}

pub async fn hello(base: &str, key: &[u8; 32], cid: &[u8; 16]) -> reqwest::Response {
    let tag = proto::hello_tag(key, cid);
    http()
        .post(format!("{base}/api/hello"))
        .json(&json!({ "cid": hex::encode(cid), "tag": hex::encode(tag) }))
        .send()
        .await
        .unwrap()
}

pub struct Client {
    pub base: String,
    pub http: reqwest::Client,
    pub cid: [u8; 16],
    pub keys: proto::ChannelKeys,
    pub ctr: u64,
}

impl Client {
    pub fn from_secret(base: &str, cid: [u8; 16], session_secret: &[u8; 32]) -> Self {
        Client { base: base.to_string(), http: http(), cid, keys: proto::ChannelKeys::derive(session_secret), ctr: 0 }
    }

    /// 用下一个计数器加密，返回 (计数器, 请求体)。
    pub fn seal(&mut self, path: &str, plain: &[u8]) -> (u64, Vec<u8>) {
        self.ctr += 1;
        (self.ctr, proto::seal(&self.keys.c2s, self.ctr, &proto::req_aad(path), plain))
    }

    /// 原样发出一个加密请求（可用于重放、篡改、换路径）。
    pub async fn post_raw(&self, path: &str, ctr: u64, body: Vec<u8>) -> (u16, Vec<u8>) {
        let resp = self
            .http
            .post(format!("{}{path}", self.base))
            .header("X-LS-Sid", hex::encode(self.cid))
            .header("X-LS-Ctr", ctr.to_string())
            .header("Content-Type", "application/octet-stream")
            .body(body)
            .send()
            .await
            .unwrap();
        (resp.status().as_u16(), resp.bytes().await.unwrap().to_vec())
    }

    /// 加密发送并解开响应；状态码不是 200 时返回 Err(状态码)。
    pub async fn send(&mut self, path: &str, plain: &[u8]) -> Result<Vec<u8>, u16> {
        let (ctr, body) = self.seal(path, plain);
        let (status, bytes) = self.post_raw(path, ctr, body).await;
        if status != 200 {
            return Err(status);
        }
        Ok(proto::open(&self.keys.s2c, ctr, &proto::resp_aad(path), &bytes).expect("响应必须能用 s2c 解开"))
    }

    pub async fn rpc(&mut self, request: Value) -> Value {
        let bytes = self.send("/api/rpc", request.to_string().as_bytes()).await.expect("rpc 应返回 200");
        serde_json::from_slice(&bytes).unwrap()
    }
}

/// 扫码配对。
pub async fn pair_qr(server: &TestServer) -> Client {
    let cid = random_cid();
    let resp = hello(&server.base, &server.key(), &cid).await;
    assert_eq!(resp.status().as_u16(), 200);
    Client::from_secret(&server.base, cid, &proto::session_secret_from_key(&server.key(), &cid))
}

/// 口令配对（SPAKE2）。服务端确认标签不对时返回 Err("wrong_pin")，HTTP 错误返回 Err(状态码)。
pub async fn pair_pin(base: &str, pin: &str, server_id: &str) -> Result<Client, String> {
    let cid = random_cid();
    let (state, msg) = proto::pake_start(pin, server_id, OsRng);
    let b64 = |b: &[u8]| base64::Engine::encode(&base64::engine::general_purpose::STANDARD, b);
    let resp = http()
        .post(format!("{base}/api/pake/start"))
        .json(&json!({ "cid": hex::encode(cid), "msg": b64(&msg) }))
        .send()
        .await
        .unwrap();
    if resp.status() != 200 {
        return Err(resp.status().as_u16().to_string());
    }
    let reply: Value = resp.json().await.unwrap();
    let peer = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, reply["msg"].as_str().unwrap()).unwrap();
    let pake_key = proto::pake_finish(state, &peer).unwrap();
    let server_tag = hex::decode(reply["confirm"].as_str().unwrap()).unwrap();
    if !proto::verify_confirm(&pake_key, "server", &cid, &server_tag) {
        return Err("wrong_pin".into());
    }
    let resp = http()
        .post(format!("{base}/api/pake/finish"))
        .json(&json!({ "cid": hex::encode(cid), "confirm": hex::encode(proto::confirm_tag(&pake_key, "client", &cid)) }))
        .send()
        .await
        .unwrap();
    if resp.status() != 200 {
        return Err(resp.status().as_u16().to_string());
    }
    Ok(Client::from_secret(base, cid, &proto::session_secret_from_pake(&pake_key, &cid)))
}

pub async fn server_id(server: &TestServer) -> String {
    server.state.server_id().to_string()
}

// ---------- 分块传输 ----------

impl Client {
    /// 完整上传一个文件：begin → 逐块发送 → finish。返回服务端保存的文件名，失败返回错误码。
    pub async fn upload(&mut self, name: &str, data: &[u8]) -> Result<String, String> {
        let begin = self.rpc(json!({ "op": "upload_begin", "name": name, "size": data.len() })).await;
        if begin["ok"] != true {
            return Err(begin["error"].as_str().unwrap_or("?").to_string());
        }
        let id = begin["upload_id"].as_str().unwrap().to_string();
        for (index, chunk) in data.chunks(proto::CHUNK).enumerate() {
            let r = self.upload_chunk(&id, index, chunk).await;
            if r["ok"] != true {
                return Err(r["error"].as_str().unwrap_or("?").to_string());
            }
        }
        let done = self.rpc(json!({ "op": "upload_finish", "upload_id": id })).await;
        if done["ok"] != true {
            return Err(done["error"].as_str().unwrap_or("?").to_string());
        }
        Ok(done["name"].as_str().unwrap().to_string())
    }

    pub async fn upload_chunk(&mut self, upload_id: &str, index: usize, chunk: &[u8]) -> Value {
        let bytes = self.send(&format!("/api/up/{upload_id}/{index}"), chunk).await.expect("上传块应返回 200");
        serde_json::from_slice(&bytes).unwrap()
    }

    /// 下载一块：Ok(数据) 或 Err(错误码)。
    pub async fn download_chunk(&mut self, file_id: &str, index: usize, expect: Value) -> Result<Vec<u8>, String> {
        let bytes = self
            .send(&format!("/api/down/{file_id}/{index}"), expect.to_string().as_bytes())
            .await
            .expect("下载块应返回 200");
        match bytes.split_first() {
            Some((0, data)) => Ok(data.to_vec()),
            Some((1, err)) => Err(serde_json::from_slice::<Value>(err).unwrap()["error"].as_str().unwrap().to_string()),
            _ => panic!("下载响应缺状态字节"),
        }
    }

    /// 按列表里的 id 和大小下载整个文件。
    pub async fn download(&mut self, file_id: &str, size: u64, mtime: f64) -> Result<Vec<u8>, String> {
        let chunks = (size as usize).div_ceil(proto::CHUNK).max(1);
        let mut out = Vec::with_capacity(size as usize);
        for index in 0..chunks {
            out.extend(self.download_chunk(file_id, index, json!({ "size": size, "mtime": mtime })).await?);
        }
        Ok(out)
    }

    pub async fn find_file(&mut self, name: &str) -> Value {
        let list = self.rpc(json!({ "op": "list" })).await;
        list["files"].as_array().unwrap().iter().find(|f| f["name"] == name).cloned().expect("列表里应该有这个文件")
    }
}
