//! 分块上传/下载。
//!
//! 上传：`upload_begin` 建一个隐藏临时文件 → 每块按偏移写入（可并发、可重发、可续传）→
//! `upload_finish` 收齐后改名落盘。上传只属于发起它的会话，别的设备碰不到。
//! 下载：每块单独请求；响应明文第一个字节是状态（0 = 后面是数据，1 = 后面是 JSON 错误）。
//!
//! 资源上限（独立安全审查 I-2）：全局和每个会话的并发上传数在**同一次加锁**里检查并占位；
//! 一段时间没有新块的上传自动作废（删临时文件、释放预占的磁盘空间）；预占空间前确认磁盘还留有余量。

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use lanshare_proto::{CHUNK, sanitize_filename};
use serde_json::{Value, json};

use super::rpc::{reply, reply_error};
use super::{AppState, random_hex};

/// 单个文件的大小上限（1 TiB）：先挡住离谱的数字，再去碰磁盘和内存。
const MAX_FILE: u64 = 1 << 40;

pub(crate) struct Upload {
    owner: [u8; 16],
    name: String,
    size: u64,
    received: Vec<bool>,
    temp: PathBuf,
    last_active: Instant,
}

#[derive(Default)]
pub(crate) struct Uploads(HashMap<String, Upload>);

fn chunk_count(size: u64) -> usize {
    size.div_ceil(CHUNK as u64) as usize
}

fn expected_len(size: u64, index: usize) -> u64 {
    let start = index as u64 * CHUNK as u64;
    (size - start).min(CHUNK as u64)
}

fn io_error_code(e: &io::Error) -> &'static str {
    match e.kind() {
        io::ErrorKind::StorageFull => "disk_full",
        io::ErrorKind::NotFound => "not_found",
        _ => "io_error",
    }
}

pub(crate) fn human_size(n: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut v = n as f64;
    for unit in UNITS {
        v /= 1024.0;
        if v < 1024.0 {
            return format!("{v:.1} {unit}");
        }
    }
    format!("{v:.1} PB")
}

// ---------- 过期清理 ----------

/// 删掉超过 `upload_idle` 没有动静的上传，返回删除个数。
pub(crate) fn reap(st: &AppState) -> usize {
    let stale: Vec<Upload> = {
        let mut uploads = st.uploads.lock().expect("锁不会中毒");
        let ids: Vec<String> =
            uploads.0.iter().filter(|(_, u)| u.last_active.elapsed() >= st.upload_idle).map(|(id, _)| id.clone()).collect();
        ids.iter().filter_map(|id| uploads.0.remove(id)).collect()
    };
    for upload in &stale {
        let _ = fs::remove_file(&upload.temp);
    }
    stale.len()
}

/// 后台定时清理。只持有弱引用：服务停了、状态释放了，这个任务也就结束了。
pub(crate) async fn reap_forever(state: Weak<AppState>) {
    loop {
        let Some(st) = state.upgrade() else { return };
        let every = (st.upload_idle / 2).clamp(Duration::from_millis(100), Duration::from_secs(30));
        drop(st);
        tokio::time::sleep(every).await;
        let Some(st) = state.upgrade() else { return };
        let removed = tokio::task::spawn_blocking(move || reap(&st)).await.unwrap_or(0);
        if removed > 0
            && let Some(st) = state.upgrade()
        {
            st.log(&format!("清理了 {removed} 个长时间没有动静的上传"));
        }
    }
}

// ---------- RPC：上传的开始、状态、完成、放弃 ----------

pub(crate) async fn begin(st: &Arc<AppState>, request: &Value, cid: [u8; 16]) -> Vec<u8> {
    let (Some(name), Some(size)) = (request["name"].as_str(), request["size"].as_u64()) else {
        return reply_error("bad_request");
    };
    if size > MAX_FILE {
        return reply_error("too_large");
    }
    let st2 = st.clone();
    let _ = tokio::task::spawn_blocking(move || reap(&st2)).await;

    // 检查名额和占位在同一次加锁里完成：并发的 begin 不可能一起越过上限
    let upload_id = random_hex(16);
    let temp = st.storage.temp_path(&upload_id);
    {
        let mut uploads = st.uploads.lock().expect("锁不会中毒");
        let mine = uploads.0.values().filter(|u| u.owner == cid).count();
        if uploads.0.len() >= st.max_uploads || mine >= st.max_uploads_per_session {
            return reply_error("too_many_uploads");
        }
        let upload = Upload {
            owner: cid,
            name: sanitize_filename(name),
            size,
            received: vec![false; chunk_count(size)],
            temp: temp.clone(),
            last_active: Instant::now(),
        };
        uploads.0.insert(upload_id.clone(), upload);
    }

    let st2 = st.clone();
    let temp2 = temp.clone();
    let reserve = st.disk_reserve;
    let created = tokio::task::spawn_blocking(move || -> io::Result<()> {
        st2.storage.ensure_root()?;
        // 查剩余空间和预占空间一个接一个做：并发的 begin 不会看到同一个剩余空间（复验 N-5）
        let _alloc = st2.alloc_lock.lock().unwrap_or_else(|e| e.into_inner());
        if st2.storage.available_space().is_some_and(|free| free < size.saturating_add(reserve)) {
            return Err(io::Error::from(io::ErrorKind::StorageFull));
        }
        let file = fs::File::create(&temp2)?;
        file.set_len(size)?; // 预先占好空间：磁盘不够时这里就会失败，而不是传到一半
        Ok(())
    })
    .await;
    let failed = match created {
        Ok(Ok(())) => None,
        Ok(Err(e)) => Some(io_error_code(&e)),
        Err(_) => Some("io_error"),
    };
    if let Some(code) = failed {
        st.uploads.lock().expect("锁不会中毒").0.remove(&upload_id);
        let _ = fs::remove_file(&temp);
        return reply_error(code);
    }
    reply(json!({ "ok": true, "upload_id": upload_id, "chunk": CHUNK }))
}

pub(crate) fn status(st: &AppState, request: &Value, cid: [u8; 16]) -> Vec<u8> {
    let uploads = st.uploads.lock().expect("锁不会中毒");
    // 只读，不刷新活动时间：只查进度、不传块，不能让上传一直占着预留的空间（复验 N-5）
    let Some(upload) = request["upload_id"].as_str().and_then(|id| uploads.0.get(id)).filter(|u| u.owner == cid)
    else {
        return reply_error("not_found");
    };
    let received: Vec<usize> = upload.received.iter().enumerate().filter(|(_, r)| **r).map(|(i, _)| i).collect();
    reply(json!({ "ok": true, "received": received }))
}

pub(crate) async fn finish(st: &Arc<AppState>, request: &Value, cid: [u8; 16], peer: IpAddr) -> Vec<u8> {
    let upload = {
        let mut uploads = st.uploads.lock().expect("锁不会中毒");
        let Some(id) = request["upload_id"].as_str() else { return reply_error("bad_request") };
        match uploads.0.get(id) {
            Some(u) if u.owner == cid => {
                if u.received.iter().any(|r| !r) {
                    return reply_error("incomplete");
                }
            }
            _ => return reply_error("not_found"),
        }
        uploads.0.remove(id).expect("刚确认过存在")
    };
    let st2 = st.clone();
    let (temp, name) = (upload.temp.clone(), upload.name.clone());
    match tokio::task::spawn_blocking(move || st2.storage.finalize(&temp, &name)).await {
        Ok(Ok(saved)) => {
            st.log(&format!("收到 {saved}（{}），来自 {peer}", human_size(upload.size)));
            reply(json!({ "ok": true, "name": saved }))
        }
        Ok(Err(e)) => {
            let _ = fs::remove_file(&upload.temp);
            reply_error(io_error_code(&e))
        }
        Err(_) => reply_error("io_error"),
    }
}

pub(crate) fn abort(st: &AppState, request: &Value, cid: [u8; 16]) -> Vec<u8> {
    let mut uploads = st.uploads.lock().expect("锁不会中毒");
    let Some(id) = request["upload_id"].as_str() else { return reply_error("bad_request") };
    match uploads.0.get(id) {
        Some(u) if u.owner == cid => {
            let upload = uploads.0.remove(id).expect("刚确认过存在");
            let _ = fs::remove_file(upload.temp);
            reply(json!({ "ok": true }))
        }
        _ => reply_error("not_found"),
    }
}

// ---------- 块 ----------

/// `/api/up/<upload_id>/<index>`：把一块写到临时文件的对应位置。
pub(crate) async fn put_chunk(st: &Arc<AppState>, upload_id: &str, index: &str, data: Vec<u8>, cid: [u8; 16]) -> Vec<u8> {
    let Ok(index) = index.parse::<usize>() else { return reply_error("bad_index") };
    let (temp, size) = {
        let mut uploads = st.uploads.lock().expect("锁不会中毒");
        let Some(upload) = uploads.0.get_mut(upload_id).filter(|u| u.owner == cid) else {
            return reply_error("not_found");
        };
        if index >= upload.received.len() {
            return reply_error("bad_index");
        }
        upload.last_active = Instant::now();
        (upload.temp.clone(), upload.size)
    };
    if data.len() as u64 != expected_len(size, index) {
        return reply_error("bad_length");
    }
    let written = tokio::task::spawn_blocking(move || -> io::Result<()> {
        let mut file = OpenOptions::new().write(true).open(&temp)?;
        file.seek(SeekFrom::Start(index as u64 * CHUNK as u64))?;
        file.write_all(&data)
    })
    .await;
    match written {
        Ok(Ok(())) => {
            let mut uploads = st.uploads.lock().expect("锁不会中毒");
            if let Some(upload) = uploads.0.get_mut(upload_id) {
                upload.received[index] = true;
                upload.last_active = Instant::now();
            }
            reply(json!({ "ok": true }))
        }
        Ok(Err(e)) => reply_error(io_error_code(&e)),
        Err(_) => reply_error("io_error"),
    }
}

fn down_error(code: &str) -> Vec<u8> {
    let mut out = vec![1u8];
    out.extend(reply_error(code));
    out
}

/// `/api/down/<file_id>/<index>`：读出一块。请求明文可带 `{size, mtime}`，文件变了就报 `changed`。
pub(crate) async fn get_chunk(st: &Arc<AppState>, file_id: &str, index: &str, plain: &[u8], peer: IpAddr) -> Vec<u8> {
    let Ok(index) = index.parse::<usize>() else { return down_error("bad_index") };
    let expect: Value = serde_json::from_slice(plain).unwrap_or(Value::Null);
    let Some(name) = st.files.lock().expect("锁不会中毒").name_for(file_id) else {
        return down_error("not_found");
    };
    let st2 = st.clone();
    let name2 = name.clone();
    let result = tokio::task::spawn_blocking(move || -> Result<Vec<u8>, &'static str> {
        let path = st2.storage.resolve(&name2).ok_or("not_found")?;
        let mut file = fs::File::open(&path).map_err(|e| io_error_code(&e))?;
        let meta = file.metadata().map_err(|e| io_error_code(&e))?;
        let size = meta.len();
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        let size_changed = expect["size"].as_u64().is_some_and(|s| s != size);
        let mtime_changed = expect["mtime"].as_f64().is_some_and(|m| (m - mtime).abs() > 1e-3);
        if size_changed || mtime_changed {
            return Err("changed");
        }
        let start = (index as u64).checked_mul(CHUNK as u64).ok_or("bad_index")?;
        if !(start < size || (size == 0 && index == 0)) {
            return Err("bad_index");
        }
        let len = if size == 0 { 0 } else { expected_len(size, index) };
        let mut out = vec![0u8; 1 + len as usize];
        file.seek(SeekFrom::Start(start)).map_err(|e| io_error_code(&e))?;
        file.read_exact(&mut out[1..]).map_err(|e| io_error_code(&e))?;
        Ok(out)
    })
    .await;
    match result {
        Ok(Ok(out)) => {
            if index == 0 {
                st.log(&format!("发送 {name} 到 {peer}"));
            }
            out
        }
        Ok(Err(code)) => down_error(code),
        Err(_) => down_error("io_error"),
    }
}
