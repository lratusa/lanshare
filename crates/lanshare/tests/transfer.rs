//! 分块上传/下载：完整性、续传、改名、越界、权限、目录自愈。

mod common;

use common::*;
use lanshare_proto as proto;
use rand_core::{OsRng, RngCore};
use serde_json::json;
use sha2::{Digest, Sha256};

fn random_bytes(n: usize) -> Vec<u8> {
    let mut v = vec![0u8; n];
    OsRng.fill_bytes(&mut v);
    v
}

#[tokio::test]
async fn upload_then_download_20_mib_is_identical() {
    let s = start().await;
    let mut c = pair_qr(&s).await;
    let data = random_bytes(20 * 1024 * 1024 + 123);
    assert_eq!(c.upload("video.mp4", &data).await.unwrap(), "video.mp4");
    let on_disk = std::fs::read(s.share().join("video.mp4")).unwrap();
    assert_eq!(Sha256::digest(&on_disk), Sha256::digest(&data), "磁盘上的文件和原文件一致");
    let f = c.find_file("video.mp4").await;
    assert_eq!(f["size"], data.len());
    let got = c.download(f["id"].as_str().unwrap(), data.len() as u64, f["mtime"].as_f64().unwrap()).await.unwrap();
    assert_eq!(Sha256::digest(&got), Sha256::digest(&data), "下载回来的和原文件一致");
    assert!(s.logs().contains("收到 video.mp4"));
}

#[tokio::test]
async fn empty_file() {
    let s = start().await;
    let mut c = pair_qr(&s).await;
    assert_eq!(c.upload("empty.txt", b"").await.unwrap(), "empty.txt");
    let f = c.find_file("empty.txt").await;
    assert_eq!(c.download(f["id"].as_str().unwrap(), 0, f["mtime"].as_f64().unwrap()).await.unwrap(), b"");
}

#[tokio::test]
async fn names_are_sanitized_and_deduplicated() {
    let s = start().await;
    let mut c = pair_qr(&s).await;
    assert_eq!(c.upload("测试 文件#1.txt", b"x").await.unwrap(), "测试 文件#1.txt");
    assert_eq!(c.upload("a.txt", b"1").await.unwrap(), "a.txt");
    assert_eq!(c.upload("a.txt", b"2").await.unwrap(), "a (1).txt");
    assert_eq!(c.upload("../../evil.txt", b"e").await.unwrap(), "evil.txt");
    assert_eq!(c.upload("a\u{202E}gpj.exe", b"m").await.unwrap(), "agpj.exe");
    let mut outside: Vec<_> = std::fs::read_dir(s.dir.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
    outside.sort();
    assert_eq!(outside, ["share"], "没有文件逃出共享目录");
}

#[tokio::test]
async fn resume_after_interruption() {
    let s = start().await;
    let mut c = pair_qr(&s).await;
    let data = random_bytes(proto::CHUNK * 2 + 10);
    let begin = c.rpc(json!({ "op": "upload_begin", "name": "big.bin", "size": data.len() })).await;
    let id = begin["upload_id"].as_str().unwrap().to_string();
    let chunks: Vec<&[u8]> = data.chunks(proto::CHUNK).collect();
    assert_eq!(c.upload_chunk(&id, 0, chunks[0]).await["ok"], true);
    assert_eq!(c.upload_chunk(&id, 2, chunks[2]).await["ok"], true);
    // “网断了”：重新问服务端收到了哪些块
    let status = c.rpc(json!({ "op": "upload_status", "upload_id": id })).await;
    assert_eq!(status["received"], json!([0, 2]));
    let early = c.rpc(json!({ "op": "upload_finish", "upload_id": id })).await;
    assert_eq!(early["error"], "incomplete", "块没到齐不能落盘");
    assert_eq!(c.upload_chunk(&id, 1, chunks[1]).await["ok"], true);
    assert_eq!(c.upload_chunk(&id, 1, chunks[1]).await["ok"], true, "重发同一块是幂等的");
    let done = c.rpc(json!({ "op": "upload_finish", "upload_id": id })).await;
    assert_eq!(done["name"], "big.bin");
    assert_eq!(Sha256::digest(std::fs::read(s.share().join("big.bin")).unwrap()), Sha256::digest(&data));
}

#[tokio::test]
async fn bad_chunks_are_rejected() {
    let s = start().await;
    let mut c = pair_qr(&s).await;
    let begin = c.rpc(json!({ "op": "upload_begin", "name": "x.bin", "size": proto::CHUNK + 5 })).await;
    let id = begin["upload_id"].as_str().unwrap().to_string();
    assert_eq!(c.upload_chunk(&id, 2, b"x").await["error"], "bad_index", "越界");
    assert_eq!(c.upload_chunk(&id, 1, b"xx").await["error"], "bad_length", "最后一块应是 5 字节");
    assert_eq!(c.upload_chunk(&id, 0, b"short").await["error"], "bad_length", "中间块必须是整块");
    assert_eq!(c.upload_chunk("ffffffffffffffffffffffffffffffff", 0, b"x").await["error"], "not_found");
}

#[tokio::test]
async fn another_device_cannot_touch_my_upload() {
    let s = start().await;
    let mut mine = pair_qr(&s).await;
    let mut other = pair_qr(&s).await;
    let begin = mine.rpc(json!({ "op": "upload_begin", "name": "x.bin", "size": 3 })).await;
    let id = begin["upload_id"].as_str().unwrap().to_string();
    assert_eq!(other.upload_chunk(&id, 0, b"bad").await["error"], "not_found");
    assert_eq!(other.rpc(json!({ "op": "upload_finish", "upload_id": id })).await["error"], "not_found");
    assert_eq!(mine.upload_chunk(&id, 0, b"abc").await["ok"], true);
}

#[tokio::test]
async fn abort_removes_the_temporary_file() {
    let s = start().await;
    let mut c = pair_qr(&s).await;
    let begin = c.rpc(json!({ "op": "upload_begin", "name": "x.bin", "size": 10 })).await;
    let id = begin["upload_id"].as_str().unwrap().to_string();
    assert_eq!(std::fs::read_dir(s.share()).unwrap().count(), 1, "临时文件已建好");
    assert_eq!(c.rpc(json!({ "op": "upload_abort", "upload_id": id })).await["ok"], true);
    assert_eq!(std::fs::read_dir(s.share()).unwrap().count(), 0);
    assert_eq!(c.rpc(json!({ "op": "upload_status", "upload_id": id })).await["error"], "not_found");
}

#[tokio::test]
async fn invalid_upload_requests() {
    let s = start().await;
    let mut c = pair_qr(&s).await;
    for bad in [json!({ "op": "upload_begin", "name": "x" }), json!({ "op": "upload_begin", "size": 3 }),
                json!({ "op": "upload_begin", "name": "x", "size": -1 })] {
        assert_eq!(c.rpc(bad.clone()).await["error"], "bad_request", "{bad}");
    }
}

#[tokio::test]
async fn download_detects_changed_files_and_bad_requests() {
    let s = start().await;
    let mut c = pair_qr(&s).await;
    c.upload("a.txt", b"hello").await.unwrap();
    let f = c.find_file("a.txt").await;
    let id = f["id"].as_str().unwrap().to_string();
    assert_eq!(c.download_chunk(&id, 0, json!({ "size": 5, "mtime": f["mtime"] })).await.unwrap(), b"hello");
    assert_eq!(c.download_chunk(&id, 0, json!({ "size": 6, "mtime": f["mtime"] })).await.unwrap_err(), "changed");
    assert_eq!(c.download_chunk(&id, 1, json!({ "size": 5, "mtime": f["mtime"] })).await.unwrap_err(), "bad_index");
    assert_eq!(c.download_chunk("0000000000000000", 0, json!({})).await.unwrap_err(), "not_found");
    std::fs::remove_file(s.share().join("a.txt")).unwrap();
    assert_eq!(c.download_chunk(&id, 0, json!({ "size": 5, "mtime": f["mtime"] })).await.unwrap_err(), "not_found");
}

#[tokio::test]
async fn share_dir_deleted_while_running_heals() {
    let s = start().await;
    let mut c = pair_qr(&s).await;
    std::fs::remove_dir_all(s.share()).unwrap();
    assert_eq!(c.rpc(json!({ "op": "list" })).await["files"], json!([]));
    std::fs::remove_dir_all(s.share()).unwrap();
    assert_eq!(c.upload("a.txt", b"1").await.unwrap(), "a.txt");
}

#[tokio::test]
async fn parallel_chunks_for_one_upload() {
    let s = start().await;
    let c = pair_qr(&s).await;
    let data = random_bytes(proto::CHUNK * 4);
    let mut c = c;
    let begin = c.rpc(json!({ "op": "upload_begin", "name": "p.bin", "size": data.len() })).await;
    let id = begin["upload_id"].as_str().unwrap().to_string();
    // 先按顺序加密好 4 块，再并发发出去（浏览器也是这样）
    let mut sealed = Vec::new();
    for (i, chunk) in data.chunks(proto::CHUNK).enumerate() {
        let path = format!("/api/up/{id}/{i}");
        let (ctr, body) = c.seal(&path, chunk);
        sealed.push((path, ctr, body));
    }
    // 倒序、真正并发地发出去（每个任务各自持有 HTTP 客户端的克隆）
    let tasks: Vec<_> = sealed
        .into_iter()
        .rev()
        .map(|(path, ctr, body)| {
            let (http, base, sid) = (c.http.clone(), c.base.clone(), hex::encode(c.cid));
            tokio::spawn(async move {
                http.post(format!("{base}{path}"))
                    .header("X-LS-Sid", sid)
                    .header("X-LS-Ctr", ctr.to_string())
                    .header("Content-Type", "application/octet-stream")
                    .body(body)
                    .send()
                    .await
                    .unwrap()
                    .status()
                    .as_u16()
            })
        })
        .collect();
    let mut statuses = Vec::new();
    for task in tasks {
        statuses.push(task.await.unwrap());
    }
    assert!(statuses.iter().all(|s| *s == 200), "{statuses:?}");
    assert_eq!(c.rpc(json!({ "op": "upload_finish", "upload_id": id })).await["name"], "p.bin");
    assert_eq!(Sha256::digest(std::fs::read(s.share().join("p.bin")).unwrap()), Sha256::digest(&data));
}
