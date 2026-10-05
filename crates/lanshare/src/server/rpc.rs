//! RPC：`/api/rpc` 里解密后的 JSON 请求。所有业务错误都在加密的 JSON 里返回。

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Instant;

use serde_json::{Value, json};

use super::{AppState, transfer};

pub(crate) fn reply(value: Value) -> Vec<u8> {
    value.to_string().into_bytes()
}

pub(crate) fn reply_error(code: &str) -> Vec<u8> {
    reply(json!({ "ok": false, "error": code }))
}

fn qr_svg(url: &str) -> String {
    match qrcode::QrCode::new(url.as_bytes()) {
        Ok(code) => code
            .render::<qrcode::render::svg::Color>()
            .min_dimensions(200, 200)
            .quiet_zone(true)
            .build(),
        Err(_) => String::new(),
    }
}

pub(crate) async fn handle(st: &Arc<AppState>, plain: &[u8], local: bool, cid: [u8; 16], peer: IpAddr) -> Vec<u8> {
    let Ok(request) = serde_json::from_slice::<Value>(plain) else {
        return reply_error("bad_request");
    };
    match request["op"].as_str().unwrap_or("") {
        "info" => info(st, local),
        "list" => list(st).await,
        "texts" => reply(json!({ "ok": true, "texts": st.texts.list() })),
        "text_add" => match request["text"].as_str().map(|t| st.texts.add(t)) {
            Some(Ok(item)) => reply(json!({ "ok": true, "item": item })),
            Some(Err(code)) => reply_error(code),
            None => reply_error("bad_request"),
        },
        "reveal" => reveal(st, &request, local).await,
        "open_folder" => open_folder(st, local),
        "pin_resume" => pin_resume(st, local),
        "approvals" => approvals(st, local),
        "approve" => decide(st, &request, local, true),
        "reject" => decide(st, &request, local, false),
        "upload_begin" => transfer::begin(st, &request, cid).await,
        "upload_status" => transfer::status(st, &request, cid),
        "upload_finish" => transfer::finish(st, &request, cid, peer).await,
        "upload_abort" => transfer::abort(st, &request, cid),
        _ => reply_error("unknown_op"),
    }
}

fn info(st: &AppState, local: bool) -> Vec<u8> {
    let mut info = json!({
        "ok": true,
        "version": env!("CARGO_PKG_VERSION"),
        "server_id": st.server_id(),
        "urls": st.base_urls(),
        "pin": st.pin(),
        "pin_paused": st.pin_paused(),
        "qr_svg": qr_svg(&st.entry_url()),
        // 已配对设备（包括用口令配对的）经加密信道拿到 K，下次打开页面可直接握手
        "key": st.key_fragment(),
        "local": local,
        "chunk": lanshare_proto::CHUNK,
    });
    if local {
        info["folder"] = json!(st.share_dir().display().to_string()); // 本机路径只告诉本机
    }
    reply(info)
}

async fn list(st: &Arc<AppState>) -> Vec<u8> {
    let st2 = st.clone();
    let files = tokio::task::spawn_blocking(move || st2.storage.list()).await.unwrap_or_default();
    let mut ids = st.files.lock().expect("锁不会中毒");
    let files: Vec<Value> = files
        .into_iter()
        .map(|f| json!({ "id": ids.id_for(&f.name), "name": f.name, "size": f.size, "mtime": f.mtime }))
        .collect();
    reply(json!({ "ok": true, "files": files }))
}

async fn reveal(st: &Arc<AppState>, request: &Value, local: bool) -> Vec<u8> {
    if !local {
        return reply_error("only_local");
    }
    let Some(desktop) = st.desktop.clone() else { return reply_error("unavailable") };
    let name = request["id"].as_str().and_then(|id| st.files.lock().expect("锁不会中毒").name_for(id));
    let Some(name) = name else { return reply_error("not_found") };
    let st2 = st.clone();
    let Ok(Some(path)) = tokio::task::spawn_blocking(move || st2.storage.resolve(&name)).await else {
        return reply_error("not_found");
    };
    desktop.reveal(&path);
    reply(json!({ "ok": true }))
}

/// 本机主界面上“重新开启口令登录”。只认本机：口令被暂停正是因为局域网里有人在猜。
fn pin_resume(st: &AppState, local: bool) -> Vec<u8> {
    if !local {
        return reply_error("only_local");
    }
    let pin = st.resume_pin();
    st.log("已重新开启口令登录，并换了新口令");
    reply(json!({ "ok": true, "pin": pin }))
}

/// 本机主界面：有哪些新设备在等确认。
fn approvals(st: &AppState, local: bool) -> Vec<u8> {
    if !local {
        return reply_error("only_local");
    }
    let devices = st.approvals.lock().expect("锁不会中毒").waiting(Instant::now());
    reply(json!({ "ok": true, "devices": devices }))
}

/// 本机主界面上点“允许”或“拒绝”。只认本机：新设备不能自己放行自己。
fn decide(st: &AppState, request: &Value, local: bool, allow: bool) -> Vec<u8> {
    if !local {
        return reply_error("only_local");
    }
    let id = request["id"].as_str().unwrap_or("");
    let Some((ip, device)) = st.approvals.lock().expect("锁不会中毒").decide(id, allow, Instant::now()) else {
        return reply_error("not_found");
    };
    st.log(&format!("{}：{ip}（{device}）", if allow { "已允许新设备" } else { "已拒绝新设备" }));
    reply(json!({ "ok": true }))
}

fn open_folder(st: &AppState, local: bool) -> Vec<u8> {
    if !local {
        return reply_error("only_local");
    }
    let Some(desktop) = st.desktop.clone() else { return reply_error("unavailable") };
    if st.storage.ensure_root().is_err() {
        return reply_error("folder_unavailable");
    }
    desktop.open_folder(st.share_dir());
    reply(json!({ "ok": true }))
}
