//! 连接管理：谁能连进来、能连几个、多久不说话就断开。
//!
//! 历程（教程第 9 章）：
//! - 独立安全审查 I-3：axum 自带的 `serve` 没给 hyper 配计时器，读请求头没有时限，
//!   只连不说话、请求头只发一半的连接会永远挂着。
//! - 第一版修复用一个全局连接上限，复验 N-1 指出：名额不分来源，局域网里一台机器就能占满，
//!   连本机界面（包括“重新开启口令登录”按钮）都进不来。
//!
//! 现在：
//! - 每个来源 IP 最多 `max_connections_per_ip` 个连接；
//! - 局域网来的连接合计最多 `max_connections` 个；本机回环来的单独一池（`max_local_connections`），
//!   局域网把名额占满也不影响本机界面；
//! - 超过上限的连接**立刻关掉**，不排队（排队等于让后来的正常用户陪着干等）；
//! - 请求头的时限交给 hyper（见 `server::serve_with_shutdown`）；这里再加一个“双向空闲”计时器，
//!   收发任何字节都重新计时，管住只连不说话、对方不收数据（慢读）的连接。

use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::net::IpAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio::time::{Instant, Sleep};

#[derive(Default)]
struct Counts {
    lan: usize,
    local: usize,
    per_ip: HashMap<IpAddr, usize>,
}

/// 连接准入：按来源 IP 和本机/局域网分别计数。
#[derive(Clone)]
pub(crate) struct Admission {
    counts: Arc<Mutex<Counts>>,
    max_lan: usize,
    max_local: usize,
    per_ip: usize,
}

impl Admission {
    pub(crate) fn new(max_lan: usize, max_local: usize, per_ip: usize) -> Self {
        Admission { counts: Arc::default(), max_lan, max_local, per_ip }
    }

    /// 放行就返回一张“票”，连接关闭（票被 drop）时自动退还名额；超过任何一个上限返回 None。
    pub(crate) fn admit(&self, ip: IpAddr) -> Option<Ticket> {
        let local = ip.is_loopback();
        let mut counts = self.counts.lock().expect("锁不会中毒");
        let mine = counts.per_ip.get(&ip).copied().unwrap_or(0);
        let pool = if local { counts.local } else { counts.lan };
        if mine >= self.per_ip || pool >= if local { self.max_local } else { self.max_lan } {
            return None;
        }
        *counts.per_ip.entry(ip).or_default() += 1;
        if local {
            counts.local += 1;
        } else {
            counts.lan += 1;
        }
        Some(Ticket { counts: self.counts.clone(), ip, local })
    }
}

pub(crate) struct Ticket {
    counts: Arc<Mutex<Counts>>,
    ip: IpAddr,
    local: bool,
}

impl Drop for Ticket {
    fn drop(&mut self) {
        let mut counts = self.counts.lock().unwrap_or_else(|e| e.into_inner());
        if self.local {
            counts.local -= 1;
        } else {
            counts.lan -= 1;
        }
        if let Some(n) = counts.per_ip.get_mut(&self.ip) {
            *n -= 1;
            if *n == 0 {
                counts.per_ip.remove(&self.ip);
            }
        }
    }
}

/// 带空闲计时的连接。连接关闭（这个值被 drop）时票自动退还。
pub(crate) struct GuardedIo {
    stream: TcpStream,
    _ticket: Ticket,
    idle: Duration,
    deadline: Pin<Box<Sleep>>,
}

impl GuardedIo {
    pub(crate) fn new(stream: TcpStream, ticket: Ticket, idle: Duration) -> Self {
        GuardedIo { stream, _ticket: ticket, idle, deadline: Box::pin(tokio::time::sleep(idle)) }
    }

    fn touch(&mut self) {
        let next = Instant::now() + self.idle;
        self.deadline.as_mut().reset(next);
    }

    /// 底层还没数据可读/可写时，看看是不是已经空闲太久了。
    fn idle_expired(&mut self, cx: &mut Context<'_>) -> bool {
        self.deadline.as_mut().poll(cx).is_ready()
    }
}

fn timed_out() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, "连接空闲太久")
}

impl AsyncRead for GuardedIo {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        match Pin::new(&mut self.stream).poll_read(cx, buf) {
            Poll::Ready(result) => {
                if result.is_ok() && buf.filled().len() > before {
                    self.touch();
                }
                Poll::Ready(result)
            }
            Poll::Pending if self.idle_expired(cx) => Poll::Ready(Err(timed_out())),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl AsyncWrite for GuardedIo {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, data: &[u8]) -> Poll<io::Result<usize>> {
        match Pin::new(&mut self.stream).poll_write(cx, data) {
            Poll::Ready(result) => {
                if matches!(result, Ok(n) if n > 0) {
                    self.touch();
                }
                Poll::Ready(result)
            }
            // 对方一直不收（慢读攻击）也算空闲
            Poll::Pending if self.idle_expired(cx) => Poll::Ready(Err(timed_out())),
            Poll::Pending => Poll::Pending,
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admission_counts_per_ip_and_per_pool() {
        let a = Admission::new(3, 1, 2);
        let lan1: IpAddr = "192.168.1.5".parse().unwrap();
        let lan2: IpAddr = "192.168.1.6".parse().unwrap();
        let local: IpAddr = "127.0.0.1".parse().unwrap();
        let t1 = a.admit(lan1).unwrap();
        let _t2 = a.admit(lan1).unwrap();
        assert!(a.admit(lan1).is_none(), "同一 IP 最多 2 个");
        let _t3 = a.admit(lan2).unwrap();
        assert!(a.admit(lan2).is_none(), "局域网池子满了（3 个）");
        let _l = a.admit(local).unwrap();
        assert!(a.admit("127.0.0.2".parse().unwrap()).is_none(), "本机池子 1 个");
        drop(t1);
        assert!(a.admit(lan1).is_some(), "票退还后名额回来");
    }
}
