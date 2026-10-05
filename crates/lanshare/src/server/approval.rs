//! 新设备确认：局域网里第一次连上来的设备，要在电脑上点“允许”才能用。
//!
//! 按 IP 记：手机刷新页面会换新会话，但 IP 不变，允许过一次，这次运行内就不用再点。
//! 本机（回环地址）来的请求不经过这里：本机主界面就是点“允许”的地方。

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// 待确认的设备多久没再来问，就当它关了页面，撤掉。
const IDLE: Duration = Duration::from_secs(60);
/// 被拒绝的设备，这么久之内再连直接拒绝，不再弹到电脑上。
const REJECT_HOLD: Duration = Duration::from_secs(120);
/// 同时待确认的设备最多几个。
const MAX_WAITING: usize = 8;
/// 主界面这么久没来问过，就当它没开着。
const DESKTOP_AWAY: Duration = Duration::from_secs(6);
/// 两次弹出主界面至少隔多久：有人反复连接，也不会把窗口弹满屏幕。
const POPUP_EVERY: Duration = Duration::from_secs(30);

/// 一个局域网请求能不能放行。
pub(crate) enum Gate {
    Allow,
    /// 等电脑上确认。`new`：这台设备刚刚登记。
    Wait { code: String, new: bool },
    Reject,
    /// 待确认的设备太多了。
    Busy,
}

struct Waiting {
    id: String,
    ip: IpAddr,
    device: &'static str,
    code: String,
    last_seen: Instant,
}

#[derive(Default)]
pub(crate) struct Approvals {
    allowed: HashSet<IpAddr>,
    rejected: HashMap<IpAddr, Instant>,
    waiting: Vec<Waiting>,
    desktop_seen: Option<Instant>,
    popped: Option<Instant>,
}

impl Approvals {
    /// 局域网设备的每个加密请求都先过这里。
    pub(crate) fn gate(&mut self, ip: IpAddr, device: &'static str, now: Instant) -> Gate {
        if self.allowed.contains(&ip) {
            return Gate::Allow;
        }
        self.rejected.retain(|_, at| now.duration_since(*at) < REJECT_HOLD);
        if self.rejected.contains_key(&ip) {
            return Gate::Reject;
        }
        self.waiting.retain(|w| now.duration_since(w.last_seen) < IDLE);
        if let Some(w) = self.waiting.iter_mut().find(|w| w.ip == ip) {
            w.last_seen = now;
            return Gate::Wait { code: w.code.clone(), new: false };
        }
        if self.waiting.len() >= MAX_WAITING {
            return Gate::Busy;
        }
        // 6 位口令是均匀分布的，取前 4 位也是均匀分布的
        let code = super::random_pin()[..4].to_string();
        self.waiting.push(Waiting { id: super::random_hex(8), ip, device, code: code.clone(), last_seen: now });
        Gate::Wait { code, new: true }
    }

    /// 刚登记了新设备时问一下：主界面没开着、最近也没弹过，就该把它弹出来。
    pub(crate) fn should_pop(&mut self, now: Instant) -> bool {
        let away = self.desktop_seen.is_none_or(|t| now.duration_since(t) > DESKTOP_AWAY);
        let quiet = self.popped.is_none_or(|t| now.duration_since(t) > POPUP_EVERY);
        if away && quiet {
            self.popped = Some(now);
        }
        away && quiet
    }

    /// 本机主界面来取待确认的设备（顺便记下：主界面开着）。
    pub(crate) fn waiting(&mut self, now: Instant) -> Vec<Value> {
        self.desktop_seen = Some(now);
        self.waiting.retain(|w| now.duration_since(w.last_seen) < IDLE);
        self.waiting
            .iter()
            .map(|w| json!({ "id": w.id, "ip": w.ip.to_string(), "device": w.device, "code": w.code }))
            .collect()
    }

    /// 电脑上点了“允许”或“拒绝”。返回那台设备的 IP 和类型；没有这一条（已经撤掉了）返回 None。
    pub(crate) fn decide(&mut self, id: &str, allow: bool, now: Instant) -> Option<(IpAddr, &'static str)> {
        let index = self.waiting.iter().position(|w| w.id == id)?;
        let w = self.waiting.remove(index);
        if allow {
            self.allowed.insert(w.ip);
        } else {
            self.rejected.insert(w.ip, now);
        }
        Some((w.ip, w.device))
    }
}

/// 从 User-Agent 粗分设备类型，只用来帮电脑前的人认出是哪台设备。User-Agent 可以随便填，不能当身份。
pub(crate) fn device_label(user_agent: &str) -> &'static str {
    // 顺序有讲究：安卓的 User-Agent 里也有 Linux，iPhone 的里也有 Mac OS X
    const KINDS: [(&str, &str); 7] = [
        ("iPhone", "iPhone"),
        ("iPad", "iPad"),
        ("Android", "安卓设备"),
        ("Windows", "Windows 电脑"),
        ("Macintosh", "Mac"),
        ("CrOS", "Chromebook"),
        ("Linux", "Linux 电脑"),
    ];
    KINDS.iter().find(|(needle, _)| user_agent.contains(needle)).map_or("未知设备", |(_, label)| label)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(last: u8) -> IpAddr {
        IpAddr::from([192, 168, 1, last])
    }

    fn code_of(gate: Gate) -> (String, bool) {
        match gate {
            Gate::Wait { code, new } => (code, new),
            _ => panic!("应该是“等确认”"),
        }
    }

    #[test]
    fn new_device_waits_with_a_stable_code() {
        let mut a = Approvals::default();
        let t = Instant::now();
        let (code, new) = code_of(a.gate(ip(7), "iPhone", t));
        assert!(new);
        assert_eq!(code.len(), 4);
        assert!(code.chars().all(|c| c.is_ascii_digit()));
        let (again, new) = code_of(a.gate(ip(7), "iPhone", t + Duration::from_secs(2)));
        assert_eq!(again, code, "同一台设备再问，验证码不变");
        assert!(!new, "也不算新登记");
        assert_eq!(a.waiting(t).len(), 1);
    }

    #[test]
    fn allow_lets_every_session_from_that_ip_through() {
        let mut a = Approvals::default();
        let t = Instant::now();
        a.gate(ip(7), "iPhone", t);
        let id = a.waiting(t)[0]["id"].as_str().unwrap().to_string();
        assert_eq!(a.decide(&id, true, t), Some((ip(7), "iPhone")));
        assert!(matches!(a.gate(ip(7), "iPhone", t), Gate::Allow));
        assert!(matches!(a.gate(ip(8), "iPhone", t), Gate::Wait { .. }), "别的 IP 照样要确认");
        assert_eq!(a.decide(&id, true, t), None, "同一条不能处理两次");
    }

    #[test]
    fn reject_holds_for_a_while_then_can_ask_again() {
        let mut a = Approvals::default();
        let t = Instant::now();
        a.gate(ip(9), "安卓设备", t);
        let id = a.waiting(t)[0]["id"].as_str().unwrap().to_string();
        a.decide(&id, false, t);
        assert!(matches!(a.gate(ip(9), "安卓设备", t + Duration::from_secs(60)), Gate::Reject));
        assert!(a.waiting(t).is_empty(), "被拒绝的设备不再出现在电脑上");
        let later = t + REJECT_HOLD + Duration::from_secs(1);
        assert!(matches!(a.gate(ip(9), "安卓设备", later), Gate::Wait { new: true, .. }));
    }

    #[test]
    fn idle_requests_expire_and_the_list_is_bounded() {
        let mut a = Approvals::default();
        let t = Instant::now();
        for last in 0..MAX_WAITING as u8 {
            a.gate(ip(last), "iPhone", t);
        }
        assert!(matches!(a.gate(ip(200), "iPhone", t), Gate::Busy));
        let later = t + IDLE + Duration::from_secs(1);
        assert!(a.waiting(later).is_empty(), "关了页面的设备过一会儿自动撤掉");
        assert!(matches!(a.gate(ip(200), "iPhone", later), Gate::Wait { new: true, .. }));
    }

    #[test]
    fn pops_up_only_when_desktop_is_away_and_not_too_often() {
        let mut a = Approvals::default();
        let t = Instant::now();
        assert!(a.should_pop(t), "主界面从没来问过：弹出来");
        assert!(!a.should_pop(t + Duration::from_secs(5)), "刚弹过，不再弹");
        let t2 = t + POPUP_EVERY + Duration::from_secs(1);
        a.waiting(t2);
        assert!(!a.should_pop(t2 + Duration::from_secs(1)), "主界面开着：不弹");
        assert!(a.should_pop(t2 + DESKTOP_AWAY + Duration::from_secs(1)), "主界面关了一阵：再弹");
    }

    #[test]
    fn device_labels() {
        let iphone = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_5 like Mac OS X) AppleWebKit/605.1.15 Safari/604.1";
        let android = "Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 Chrome/126.0 Mobile Safari/537.36";
        let windows = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/126.0 Safari/537.36 Edg/126.0";
        assert_eq!(device_label(iphone), "iPhone");
        assert_eq!(device_label(android), "安卓设备");
        assert_eq!(device_label(windows), "Windows 电脑");
        assert_eq!(device_label(""), "未知设备");
    }
}
