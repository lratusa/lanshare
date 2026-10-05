//! 防重放：滑动窗口。
//!
//! 浏览器会并发发请求（轮询 + 3 个上传块），到达顺序不一定等于计数器顺序，
//! 所以不能要求“严格递增”，而是记住最近 4096 个计数器里哪些已经用过。

use std::collections::BTreeSet;

/// 窗口大小：比最高计数器小 4096 及以上的请求一律拒绝。
pub const REPLAY_WINDOW: u64 = 4096;

#[derive(Debug, Default, Clone)]
pub struct ReplayWindow {
    highest: u64,
    seen: BTreeSet<u64>,
}

impl ReplayWindow {
    pub fn new() -> Self {
        Self::default()
    }

    /// 这个计数器现在能不能用？只检查，不登记——解密成功后再 [`commit`](Self::commit)，
    /// 否则攻击者发一堆伪造请求就能把合法计数器“占”掉。
    pub fn check(&self, ctr: u64) -> bool {
        if ctr == 0 {
            return false;
        }
        if self.highest >= REPLAY_WINDOW && ctr <= self.highest - REPLAY_WINDOW {
            return false;
        }
        !self.seen.contains(&ctr)
    }

    /// 登记一个已经成功使用的计数器，并丢掉窗口外的旧记录。
    pub fn commit(&mut self, ctr: u64) {
        self.seen.insert(ctr);
        if ctr > self.highest {
            self.highest = ctr;
            let floor = self.highest.saturating_sub(REPLAY_WINDOW);
            self.seen = self.seen.split_off(&(floor + 1));
        }
    }

    /// 当前记住的计数器个数（测试用，确认内存有界）。
    pub fn tracked(&self) -> usize {
        self.seen.len()
    }
}
