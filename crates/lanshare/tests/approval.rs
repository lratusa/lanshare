//! 新设备确认：局域网里的设备要在电脑上点“允许”才能用（设计：docs/superpowers/specs/2026-10-03-device-approval.md）。
//! 用本机的局域网地址连进来，扮演“另一台设备”；本机没有局域网地址时跳过。

mod common;

use std::net::Ipv4Addr;

use common::*;
use lanshare_proto as proto;
use serde_json::json;

const IPHONE: &str = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_5 like Mac OS X) AppleWebKit/605.1.15 Mobile/15E148 Safari/604.1";

/// 在 0.0.0.0 上启动，返回服务端、局域网地址和一台扫码握过手的“手机”。
async fn lan_phone() -> Option<(TestServer, Ipv4Addr, Client)> {
    let lan_ip = lanshare::netinfo::lan_ips().into_iter().next()?;
    let s = start_with(Ipv4Addr::UNSPECIFIED, |c| c.approve_new_devices = true).await;
    let phone = scan(&s, lan_ip).await;
    Some((s, lan_ip, phone))
}

/// 从局域网地址扫码握手（相当于手机打开或刷新一次页面）。
async fn scan(s: &TestServer, lan_ip: Ipv4Addr) -> Client {
    let base = format!("http://{lan_ip}:{}", s.port);
    let cid = random_cid();
    assert_eq!(hello(&base, &s.key(), &cid).await.status(), 200, "握手和原来一样会成功");
    let mut phone = Client::from_secret(&base, cid, &proto::session_secret_from_key(&s.key(), &cid));
    phone.http = reqwest::Client::builder().no_proxy().user_agent(IPHONE).build().unwrap();
    phone
}

/// 本机主界面开着；局域网设备先发一个请求（登记成“待确认”），再在主界面上点“允许”。
async fn approve_from_desktop(s: &TestServer, device: &mut Client) {
    let mut desktop = pair_qr(s).await;
    desktop.rpc(json!({ "op": "approvals" })).await; // 主界面开着，不会弹窗
    assert_eq!(device.rpc(json!({ "op": "info" })).await["error"], "pending_approval");
    let list = desktop.rpc(json!({ "op": "approvals" })).await;
    let id = list["devices"][0]["id"].as_str().expect("电脑上应该看到这台设备").to_string();
    assert_eq!(desktop.rpc(json!({ "op": "approve", "id": id })).await["ok"], true);
}

macro_rules! need_lan {
    ($e:expr) => {
        match $e {
            Some(v) => v,
            None => {
                eprintln!("本机没有局域网地址，跳过");
                return;
            }
        }
    };
}

#[test]
fn on_by_default() {
    // 测试工具为了不牵连别的测试把它关掉了；正式运行（Config::new）必须是打开的
    assert!(lanshare::server::Config::new("share").approve_new_devices);
}

#[tokio::test]
async fn new_device_waits_until_the_desktop_allows_it() {
    let (s, lan_ip, mut phone) = need_lan!(lan_phone().await);
    let waiting = phone.rpc(json!({ "op": "info" })).await;
    assert_eq!(waiting["error"], "pending_approval");
    assert!(waiting.get("key").is_none(), "放行之前什么都不给");
    let code = waiting["code"].as_str().unwrap().to_string();
    for request in [json!({ "op": "list" }), json!({ "op": "texts" }), json!({ "op": "text_add", "text": "偷偷发一条" })] {
        assert_eq!(phone.rpc(request).await["error"], "pending_approval");
    }
    let (ctr, body) = phone.seal("/api/down/0123456789abcdef/0", b"{}");
    assert_eq!(phone.post_raw("/api/down/0123456789abcdef/0", ctr, body).await.0, 403, "分块接口直接拒绝");

    let mut desktop = pair_qr(&s).await;
    let list = desktop.rpc(json!({ "op": "approvals" })).await;
    let devices = list["devices"].as_array().unwrap();
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0]["code"], code.as_str(), "电脑上的验证码和手机上的一样");
    assert_eq!(devices[0]["device"], "iPhone");
    assert_eq!(devices[0]["ip"], lan_ip.to_string());
    let id = devices[0]["id"].as_str().unwrap();
    assert_eq!(desktop.rpc(json!({ "op": "approve", "id": id })).await["ok"], true);

    let info = phone.rpc(json!({ "op": "info" })).await;
    assert_eq!(info["ok"], true);
    assert!(info["key"].is_string(), "放行以后才拿得到 K");
    let mut refreshed = scan(&s, lan_ip).await;
    assert_eq!(refreshed.rpc(json!({ "op": "list" })).await["ok"], true, "刷新页面换了新会话，不用再点");
    assert!(desktop.rpc(json!({ "op": "approvals" })).await["devices"].as_array().unwrap().is_empty());
    assert!(s.logs().contains(&format!("已允许新设备：{lan_ip}（iPhone）")));
}

#[tokio::test]
async fn pin_paired_device_also_needs_approval() {
    let (s, lan_ip, _) = need_lan!(lan_phone().await);
    let base = format!("http://{lan_ip}:{}", s.port);
    let mut phone = pair_pin(&base, &s.state.pin(), &server_id(&s).await).await.expect("口令对了，配对照常成功");
    approve_from_desktop(&s, &mut phone).await;
    assert_eq!(phone.rpc(json!({ "op": "info" })).await["ok"], true);
}

#[tokio::test]
async fn rejected_device_stays_out() {
    let (s, lan_ip, mut phone) = need_lan!(lan_phone().await);
    phone.rpc(json!({ "op": "info" })).await;
    let mut desktop = pair_qr(&s).await;
    let id = desktop.rpc(json!({ "op": "approvals" })).await["devices"][0]["id"].as_str().unwrap().to_string();
    assert_eq!(desktop.rpc(json!({ "op": "reject", "id": id })).await["ok"], true);
    assert_eq!(phone.rpc(json!({ "op": "info" })).await["error"], "rejected");
    let mut again = scan(&s, lan_ip).await;
    assert_eq!(again.rpc(json!({ "op": "info" })).await["error"], "rejected", "重新扫码也不行");
    assert!(desktop.rpc(json!({ "op": "approvals" })).await["devices"].as_array().unwrap().is_empty(), "不再打扰电脑");
    assert_eq!(desktop.rpc(json!({ "op": "approve", "id": id })).await["error"], "not_found", "已经处理过的不能再改");
}

#[tokio::test]
async fn only_the_desktop_can_decide() {
    let (s, _, mut phone) = need_lan!(lan_phone().await);
    assert_eq!(phone.rpc(json!({ "op": "approvals" })).await["error"], "pending_approval", "没放行的设备什么都做不了");
    assert_eq!(phone.rpc(json!({ "op": "approve", "id": "x" })).await["error"], "pending_approval", "也不能放行自己");
    approve_from_desktop(&s, &mut phone).await;
    assert_eq!(phone.rpc(json!({ "op": "approvals" })).await["error"], "only_local", "放行过的设备也不能替电脑做决定");
    assert_eq!(phone.rpc(json!({ "op": "reject", "id": "x" })).await["error"], "only_local");
}

#[tokio::test]
async fn desktop_pops_up_only_when_nobody_is_watching() {
    let (s, lan_ip, mut phone) = need_lan!(lan_phone().await);
    phone.rpc(json!({ "op": "info" })).await;
    let calls = s.desktop.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 1, "主界面没开：弹出来");
    assert_eq!(calls[0].0, "app");
    assert_eq!(calls[0].1.to_str().unwrap(), s.state.local_entry_url());

    // 主界面开着（刚来问过）：不弹
    let quiet = start_with(Ipv4Addr::UNSPECIFIED, |c| c.approve_new_devices = true).await;
    pair_qr(&quiet).await.rpc(json!({ "op": "approvals" })).await;
    scan(&quiet, lan_ip).await.rpc(json!({ "op": "info" })).await;
    assert!(quiet.desktop.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn loopback_needs_no_approval() {
    let s = start_with(Ipv4Addr::LOCALHOST, |c| c.approve_new_devices = true).await;
    let mut c = pair_qr(&s).await;
    assert_eq!(c.rpc(json!({ "op": "info" })).await["ok"], true, "本机主界面不用确认");
    assert!(!s.logs().contains("新设备请求连接"), "本机连接不会登记成待确认");
    assert!(s.desktop.calls.lock().unwrap().is_empty(), "也不会弹窗");
}
