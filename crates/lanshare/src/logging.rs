//! 日志：写到 `%LOCALAPPDATA%\LanShare\lanshare.log`（超过 1 MiB 就从头开始），有控制台时同时打印。
//! 调用方负责不把秘密写进日志（服务端测试 `logs_never_contain_secrets` 守着这一点）。

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

pub const MAX_BYTES: u64 = 1024 * 1024;

pub struct FileLog {
    path: PathBuf,
    echo: bool,
    lock: Mutex<()>,
}

impl FileLog {
    pub fn new(path: PathBuf, echo: bool) -> Self {
        if let Some(dir) = path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        if fs::metadata(&path).map(|m| m.len() > MAX_BYTES).unwrap_or(false) {
            let _ = fs::remove_file(&path);
        }
        FileLog { path, echo, lock: Mutex::new(()) }
    }

    pub fn write(&self, line: &str) {
        let now = local_time();
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&self.path) {
            let _ = writeln!(f, "[{now}] {line}");
        }
        if self.echo {
            println!("{line}");
        }
    }
}

/// 本地时间，形如 `2026-10-02 09:15:21`。
#[cfg(windows)]
pub fn local_time() -> String {
    use windows_sys::Win32::System::SystemInformation::GetLocalTime;
    // SAFETY: GetLocalTime 只往这个本地结构体里写数据。
    let t = unsafe {
        let mut t = std::mem::zeroed();
        GetLocalTime(&mut t);
        t
    };
    format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}", t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond)
}

#[cfg(not(windows))]
pub fn local_time() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    format!("unix {secs}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_lines_and_trims_big_old_log() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("lanshare.log");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, vec![b'x'; (MAX_BYTES + 10) as usize]).unwrap();
        let log = FileLog::new(path.clone(), false);
        log.write("收到 a.txt");
        log.write("发送 b.txt");
        let text = fs::read_to_string(&path).unwrap();
        assert!(!text.contains("xxxx"), "超过上限的旧日志被清掉");
        assert!(text.contains("收到 a.txt") && text.contains("发送 b.txt"));
        assert_eq!(text.lines().count(), 2);
    }

    #[test]
    #[cfg(windows)]
    fn local_time_is_human_readable() {
        let t = local_time();
        assert_eq!(t.len(), 19, "{t}");
        assert!(t.starts_with("20") && &t[4..5] == "-" && &t[10..11] == " " && &t[13..14] == ":", "{t}");
    }

    #[test]
    fn creates_missing_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a").join("b").join("lanshare.log");
        FileLog::new(path.clone(), false).write("hi");
        assert!(path.exists());
    }
}
