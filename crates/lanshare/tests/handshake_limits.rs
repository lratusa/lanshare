//! 拿到 K 的人还能捣什么乱：反复扫码握手、不停地开新会话。
//! 会话表满了，每开一个新会话就挤掉一个旧的，旧会话号要记进 retired（防止重放旧握手“复活”会话），
//! 这次运行内一直占着内存。所以握手成功、开新会话的次数也有上限：每个 IP、全局，各自按 60 秒计数。

mod common;

use std::net::Ipv4Addr;

use common::*;
use lanshare_proto as proto;
use serde_json::json;

#[test]
fn limits_by_default() {
    let config = lanshare::server::Config::new("share");
    assert_eq!((config.new_sessions_per_ip, config.new_sessions_global), (30, 300));
}

#[tokio::test]
async fn new_sessions_per_ip_are_limited() {
    let s = start_with(Ipv4Addr::LOCALHOST, |c| c.new_sessions_per_ip = 3).await;
    let mut first = pair_qr(&s).await;
    pair_qr(&s).await;
    pair_qr(&s).await;
    let resp = hello(&s.base, &s.key(), &random_cid()).await;
    assert_eq!(resp.status(), 429, "这一分钟里第 4 个新会话");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error"], "too_many_sessions");
    assert_eq!(hello(&s.base, &s.key(), &first.cid).await.status(), 200, "同一个会话号再握手，不算新开");
    assert_eq!(first.rpc(json!({ "op": "info" })).await["ok"], true, "已有的会话照常能用");
}

#[tokio::test]
async fn new_sessions_are_limited_globally() {
    let s = start_with(Ipv4Addr::LOCALHOST, |c| {
        c.new_sessions_per_ip = 100;
        c.new_sessions_global = 2;
    })
    .await;
    pair_qr(&s).await;
    pair_qr(&s).await;
    assert_eq!(hello(&s.base, &s.key(), &random_cid()).await.status(), 429);
}

#[tokio::test]
async fn refused_handshakes_do_not_push_out_live_sessions() {
    let s = start_with(Ipv4Addr::LOCALHOST, |c| {
        c.max_sessions = 2;
        c.new_sessions_per_ip = 2;
    })
    .await;
    let mut a = pair_qr(&s).await;
    let mut b = pair_qr(&s).await;
    for _ in 0..5 {
        let cid = random_cid();
        assert_eq!(hello(&s.base, &s.key(), &cid).await.status(), 429);
        let mut ghost = Client::from_secret(&s.base, cid, &proto::session_secret_from_key(&s.key(), &cid));
        assert_eq!(ghost.send("/api/rpc", br#"{"op":"info"}"#).await.err(), Some(401), "被拒的握手没有建会话");
    }
    assert_eq!(a.rpc(json!({ "op": "info" })).await["ok"], true, "会话表满了，被拒的握手也挤不掉已有的会话");
    assert_eq!(b.rpc(json!({ "op": "info" })).await["ok"], true);
}
