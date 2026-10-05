//! 文字消息板：只存在内存里，保留最近 50 条。

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

pub const MAX_MESSAGES: usize = 50;
pub const MAX_TEXT_CHARS: usize = 10_000;

#[derive(Debug, Clone, Serialize)]
pub struct TextItem {
    pub id: u64,
    pub text: String,
    /// Unix 秒
    pub time: f64,
}

#[derive(Default)]
pub struct TextBoard {
    inner: Mutex<(u64, VecDeque<TextItem>)>,
}

impl TextBoard {
    pub fn new() -> Self {
        Self::default()
    }

    /// 新增一条；空白或超过 10000 字时返回错误码字符串。
    pub fn add(&self, text: &str) -> Result<TextItem, &'static str> {
        if text.trim().is_empty() {
            return Err("empty_text");
        }
        if text.chars().count() > MAX_TEXT_CHARS {
            return Err("text_too_long");
        }
        let time = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
        let mut guard = self.inner.lock().expect("文字板的锁不会中毒");
        let (next_id, items) = &mut *guard;
        *next_id += 1;
        let item = TextItem { id: *next_id, text: text.to_string(), time };
        if items.len() == MAX_MESSAGES {
            items.pop_front();
        }
        items.push_back(item.clone());
        Ok(item)
    }

    /// 全部消息，新的在前。
    pub fn list(&self) -> Vec<TextItem> {
        let guard = self.inner.lock().expect("文字板的锁不会中毒");
        guard.1.iter().rev().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newest_first_and_bounded() {
        let board = TextBoard::new();
        for i in 0..(MAX_MESSAGES + 10) {
            board.add(&format!("m{i}")).unwrap();
        }
        let list = board.list();
        assert_eq!(list.len(), MAX_MESSAGES);
        assert_eq!(list[0].text, format!("m{}", MAX_MESSAGES + 9));
    }

    #[test]
    fn rejects_blank_and_too_long() {
        let board = TextBoard::new();
        assert_eq!(board.add("  \n").unwrap_err(), "empty_text");
        assert_eq!(board.add(&"字".repeat(MAX_TEXT_CHARS + 1)).unwrap_err(), "text_too_long");
        assert!(board.add(&"字".repeat(MAX_TEXT_CHARS)).is_ok(), "按字符数而不是字节数计算");
    }
}
