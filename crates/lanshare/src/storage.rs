//! 共享文件夹：列文件、按名字安全地定位文件、清理残留临时文件。
//! 分块上传：先写隐藏临时文件，收齐后改成正式名字（重名自动加序号）。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const TEMP_PREFIX: &str = ".lanshare-";
pub const TEMP_SUFFIX: &str = ".part";
const STALE_TEMP: Duration = Duration::from_secs(600);

#[derive(Debug, Clone)]
pub struct FileEntry {
    pub name: String,
    pub size: u64,
    /// Unix 秒
    pub mtime: f64,
}

pub struct Storage {
    root: PathBuf,
    /// 选名 + 改名必须原子：两个同名上传同时完成时不能互相覆盖。
    rename_lock: Mutex<()>,
}

/// `名字.ext`、`名字 (1).ext`、`名字 (2).ext`……
fn candidate_names(name: &str) -> impl Iterator<Item = String> + '_ {
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => name.split_at(i),
        _ => (name, ""),
    };
    std::iter::once(name.to_string()).chain((1u32..).map(move |n| format!("{stem} ({n}){ext}")))
}

fn unix_seconds(t: SystemTime) -> f64 {
    t.duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

impl Storage {
    /// 不会失败：目录不存在时先试着建，之后每次用到都会再确认一次。
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let root = std::path::absolute(&root).unwrap_or(root);
        let _ = fs::create_dir_all(&root);
        Storage { root, rename_lock: Mutex::new(()) }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 运行中目录被人删了就重建。
    pub fn ensure_root(&self) -> io::Result<()> {
        fs::create_dir_all(&self.root)
    }

    fn canonical_root(&self) -> io::Result<PathBuf> {
        self.ensure_root()?;
        fs::canonicalize(&self.root)
    }

    /// 文件列表，新的在前。不含隐藏文件、临时文件、子目录、指向目录外的链接。
    pub fn list(&self) -> Vec<FileEntry> {
        let Ok(root) = self.canonical_root() else { return Vec::new() };
        let Ok(entries) = fs::read_dir(&self.root) else { return Vec::new() };
        let mut files: Vec<FileEntry> = entries
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let name = entry.file_name().into_string().ok()?;
                if name.starts_with('.') {
                    return None;
                }
                let real = fs::canonicalize(entry.path()).ok()?;
                if real.parent()? != root {
                    return None;
                }
                let meta = fs::metadata(&real).ok()?;
                if !meta.is_file() {
                    return None;
                }
                Some(FileEntry { name, size: meta.len(), mtime: meta.modified().map(unix_seconds).unwrap_or(0.0) })
            })
            .collect();
        files.sort_by(|a, b| b.mtime.total_cmp(&a.mtime));
        files
    }

    /// 共享目录里名为 `name` 的普通文件的真实路径；名字可疑、不存在、越界时返回 None。
    pub fn resolve(&self, name: &str) -> Option<PathBuf> {
        if name.is_empty() || name.starts_with('.') || name.contains(['/', '\\', ':', '\0']) {
            return None;
        }
        let root = self.canonical_root().ok()?;
        let real = fs::canonicalize(self.root.join(name)).ok()?;
        if real.parent()? != root || !fs::metadata(&real).ok()?.is_file() {
            return None;
        }
        Some(real)
    }

    /// 某次上传的临时文件路径（隐藏，列表里看不到）。
    pub fn temp_path(&self, upload_id: &str) -> PathBuf {
        self.root.join(format!("{TEMP_PREFIX}{upload_id}{TEMP_SUFFIX}"))
    }

    /// 把收齐的临时文件改成正式名字，重名自动加序号，返回实际文件名。
    pub fn finalize(&self, temp: &Path, name: &str) -> io::Result<String> {
        let _guard = self.rename_lock.lock().unwrap_or_else(|e| e.into_inner());
        for candidate in candidate_names(name).take(10_000) {
            // 不先查 exists()：“先查再改”之间别的程序可能正好建了同名文件。直接尝试不覆盖的改名，被占了就换下一个
            match rename_no_replace(temp, &self.root.join(&candidate)) {
                Ok(()) => return Ok(candidate),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
        Err(io::Error::other("找不到可用的文件名"))
    }

    /// 共享文件夹所在磁盘的剩余空间（字节）；查不到时返回 None。
    pub fn available_space(&self) -> Option<u64> {
        free_space(&self.root)
    }

    /// 删掉上次异常退出留下的临时文件（10 分钟没动过的），返回删除个数。
    pub fn cleanup_stale_temp(&self) -> usize {
        let Ok(entries) = fs::read_dir(&self.root) else { return 0 };
        let now = SystemTime::now();
        entries
            .filter_map(Result::ok)
            .filter(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                name.starts_with(TEMP_PREFIX) && name.ends_with(TEMP_SUFFIX)
            })
            .filter(|e| {
                e.metadata()
                    .and_then(|m| m.modified())
                    .map(|t| now.duration_since(t).unwrap_or_default() > STALE_TEMP)
                    .unwrap_or(false)
            })
            .filter(|e| fs::remove_file(e.path()).is_ok())
            .count()
    }
}

/// 改名，但**绝不覆盖**已存在的目标（独立安全审查 M-3）。
/// 注意：Rust 的 `fs::rename` 在 Windows 上会直接覆盖已存在的文件（实测确认），不能拿来做这件事。
#[cfg(windows)]
pub fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::MoveFileExW;
    let wide = |p: &Path| p.as_os_str().encode_wide().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let (a, b) = (wide(from), wide(to));
    // SAFETY: 两个路径都是以 0 结尾的 UTF-16，调用期间有效；标志 0 = 不覆盖、不跨卷复制。
    if unsafe { MoveFileExW(a.as_ptr(), b.as_ptr(), 0) } != 0 { Ok(()) } else { Err(io::Error::last_os_error()) }
}

#[cfg(not(windows))]
pub fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    fs::hard_link(from, to)?; // 目标已存在时失败（AlreadyExists）
    fs::remove_file(from)
}

#[cfg(windows)]
fn free_space(dir: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let wide: Vec<u16> = dir.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let mut free: u64 = 0;
    // SAFETY: 路径以 0 结尾、调用期间有效；只请求“调用者可用字节数”，另外两个输出传空指针。
    let ok = unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut free, std::ptr::null_mut(), std::ptr::null_mut()) };
    (ok != 0).then_some(free)
}

#[cfg(not(windows))]
fn free_space(_dir: &Path) -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, Storage) {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::new(dir.path().join("share"));
        (dir, storage)
    }

    #[test]
    fn list_hides_temp_hidden_and_dirs() {
        let (_d, s) = setup();
        fs::write(s.root().join(".lanshare-x.part"), b"t").unwrap();
        fs::write(s.root().join(".hidden"), b"h").unwrap();
        fs::create_dir(s.root().join("folder")).unwrap();
        fs::write(s.root().join("ok.txt"), b"1").unwrap();
        let names: Vec<_> = s.list().into_iter().map(|f| f.name).collect();
        assert_eq!(names, ["ok.txt"]);
    }

    #[test]
    fn resolve_rejects_escapes() {
        let (d, s) = setup();
        fs::write(s.root().join("a.txt"), b"1").unwrap();
        fs::write(d.path().join("secret.txt"), b"s").unwrap();
        fs::write(s.root().join(".lanshare-x.part"), b"t").unwrap();
        assert!(s.resolve("a.txt").is_some());
        for bad in ["", "missing.txt", "../secret.txt", "..\\secret.txt", "..", ".", ".lanshare-x.part", "a.txt:stream", "CON"] {
            assert!(s.resolve(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn finalize_numbers_duplicates() {
        let (_d, s) = setup();
        for (i, expected) in ["a.txt", "a (1).txt", "a (2).txt"].iter().enumerate() {
            let temp = s.temp_path(&format!("u{i}"));
            fs::write(&temp, [i as u8]).unwrap();
            assert_eq!(s.finalize(&temp, "a.txt").unwrap(), *expected);
        }
        let temp = s.temp_path("noext");
        fs::write(&temp, b"x").unwrap();
        fs::write(s.root().join("README"), b"x").unwrap();
        assert_eq!(s.finalize(&temp, "README").unwrap(), "README (1)");
    }

    #[test]
    fn rename_never_overwrites() {
        let (_d, s) = setup();
        let (from, to) = (s.root().join("from.part"), s.root().join("taken.txt"));
        fs::write(&from, b"new").unwrap();
        fs::write(&to, b"original").unwrap();
        let err = rename_no_replace(&from, &to).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&to).unwrap(), b"original", "已存在的文件不能被覆盖");
        assert!(from.exists());
        let free = s.root().join("free.txt");
        rename_no_replace(&from, &free).unwrap();
        assert_eq!(fs::read(&free).unwrap(), b"new");
        assert!(!from.exists());
    }

    #[test]
    #[cfg(windows)]
    fn reports_free_space() {
        let (_d, s) = setup();
        assert!(s.available_space().is_some_and(|n| n > 0));
    }

    #[test]
    fn recreates_deleted_root() {
        let (_d, s) = setup();
        fs::remove_dir_all(s.root()).unwrap();
        assert!(s.list().is_empty());
        assert!(s.root().is_dir());
    }
}
