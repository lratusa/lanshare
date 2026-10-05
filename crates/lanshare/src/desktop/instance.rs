//! 单实例：命名互斥量判断“是不是已经有一个在运行”，实例记录告诉后来者“它在哪个端口、密钥是什么”。
//!
//! v1（Python）只靠实例记录 + 连接探测：电脑重启后记录残留，探测要等 1 秒超时，
//! 这 1 秒里连点两次就会启动两个。互斥量由系统管理，进程一死自动释放，不存在“残留”。

use std::fs;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceRecord {
    pub port: u16,
    /// 二维码密钥（base64url）。记录在 `%LOCALAPPDATA%` 下，只有本用户能读。
    pub key: String,
    pub pid: u32,
}

/// 先写临时文件再改名，读的一方永远不会读到写了一半的文件。
pub fn write_record(path: &Path, record: &InstanceRecord) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_vec(record)?)?;
    fs::rename(&tmp, path)
}

pub fn read_record(path: &Path) -> Option<InstanceRecord> {
    let record: InstanceRecord = serde_json::from_slice(&fs::read(path).ok()?).ok()?;
    (record.port != 0 && record.key.len() == 43).then_some(record)
}

/// 只删本进程写的记录：别的实例写的不能动。
pub fn remove_record_if_own(path: &Path, pid: u32) {
    if read_record(path).is_some_and(|r| r.pid == pid) {
        let _ = fs::remove_file(path);
    }
}

/// 进程级的命名互斥量。持有期间别的进程拿不到；进程退出时系统自动释放。
pub struct InstanceLock {
    #[cfg(windows)]
    handle: windows_sys::Win32::Foundation::HANDLE,
}

// SAFETY: 互斥量句柄只用来在 Drop 时关闭，可以在线程间移动。
unsafe impl Send for InstanceLock {}

impl InstanceLock {
    /// 拿到了返回 Some；已经有别的实例拿着返回 None。系统调用失败时宁可当作“拿到了”，不挡住启动。
    #[cfg(windows)]
    pub fn acquire(name: &str) -> Option<InstanceLock> {
        use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError};
        use windows_sys::Win32::System::Threading::CreateMutexW;
        let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: wide 是以 0 结尾的 UTF-16 字符串，在调用期间一直有效；安全属性传空指针表示默认。
        let handle = unsafe { CreateMutexW(std::ptr::null(), 0, wide.as_ptr()) };
        // SAFETY: 紧跟在 CreateMutexW 之后读取本线程的错误码。
        let already = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        if already {
            // SAFETY: handle 是刚刚拿到的有效句柄，关闭后不再使用。
            unsafe { CloseHandle(handle) };
            return None;
        }
        Some(InstanceLock { handle })
    }

    #[cfg(not(windows))]
    pub fn acquire(_name: &str) -> Option<InstanceLock> {
        Some(InstanceLock {})
    }
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        #[cfg(windows)]
        if !self.handle.is_null() {
            // SAFETY: handle 来自 CreateMutexW，只在这里关闭一次。
            unsafe { windows_sys::Win32::Foundation::CloseHandle(self.handle) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(pid: u32) -> InstanceRecord {
        InstanceRecord { port: 8000, key: "A".repeat(43), pid }
    }

    #[test]
    fn record_roundtrip_and_validation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("LanShare").join("instance.json");
        assert_eq!(read_record(&path), None);
        write_record(&path, &record(1)).unwrap();
        assert_eq!(read_record(&path), Some(record(1)));
        assert!(!path.with_extension("json.tmp").exists(), "临时文件已改名");
        for garbage in ["not json", "{}", r#"{"port":0,"key":"x","pid":1}"#, r#"{"port":80,"key":"short","pid":1}"#] {
            fs::write(&path, garbage).unwrap();
            assert_eq!(read_record(&path), None, "{garbage}");
        }
    }

    #[test]
    fn remove_only_own_record() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("instance.json");
        write_record(&path, &record(42)).unwrap();
        remove_record_if_own(&path, 7);
        assert!(path.exists(), "别人的记录不能删");
        remove_record_if_own(&path, 42);
        assert!(!path.exists());
        remove_record_if_own(&path, 42); // 不存在时也不报错
    }

    #[test]
    fn named_lock_is_exclusive_until_dropped() {
        let name = format!("Local\\LanShare-test-{}-{}", std::process::id(), line!());
        let first = InstanceLock::acquire(&name);
        assert!(first.is_some());
        if cfg!(windows) {
            assert!(InstanceLock::acquire(&name).is_none(), "第二个拿不到");
        }
        drop(first);
        assert!(InstanceLock::acquire(&name).is_some(), "释放后又能拿到");
    }
}
