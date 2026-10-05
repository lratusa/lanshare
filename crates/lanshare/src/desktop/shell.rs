//! 和 Windows 外壳打交道：找浏览器开应用窗口、在资源管理器里定位文件、弹窗。

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// `canonicalize` 在 Windows 上返回 `\\?\C:\...` 这种“原样路径”，资源管理器不认，去掉前缀。
pub fn display_path(path: &Path) -> PathBuf {
    let s = path.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        path.to_path_buf()
    }
}

/// 支持 `--app` 应用模式的浏览器候选：Edge 优先（Windows 10/11 自带），其次 Chrome。
pub fn browser_candidates() -> Vec<PathBuf> {
    let bases: Vec<PathBuf> = ["ProgramFiles(x86)", "ProgramFiles", "LOCALAPPDATA"]
        .iter()
        .filter_map(|v| std::env::var_os(v).map(PathBuf::from))
        .collect();
    let edge = bases.iter().map(|b| b.join(r"Microsoft\Edge\Application\msedge.exe"));
    let chrome = bases.iter().map(|b| b.join(r"Google\Chrome\Application\chrome.exe"));
    edge.chain(chrome).collect()
}

pub fn find_app_browser(candidates: &[PathBuf], exists: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    candidates.iter().find(|p| exists(p)).cloned()
}

/// 应用窗口的命令行参数（没有地址栏和标签页）。
pub fn app_window_args(url: &str) -> Vec<String> {
    vec![format!("--app={url}"), "--window-size=1280,880".to_string()]
}

/// 资源管理器 `/select` 的参数原文。不能交给 Rust 的自动转义：explorer 有自己的一套解析规则。
pub fn explorer_select_arg(path: &Path) -> String {
    format!("/select,\"{}\"", display_path(path).display())
}

fn explorer_exe() -> PathBuf {
    let root = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    // 绝对路径：Windows 上按名字找程序时，会先找本程序 exe 所在的目录，那里的同名程序会冒充 explorer
    root.join("explorer.exe")
}

fn spawn_quiet(command: &mut Command) -> std::io::Result<()> {
    command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().map(|_| ())
}

/// 打开主界面：有 Edge/Chrome 就用应用窗口，否则用默认浏览器。
pub fn open_app_window(url: &str) {
    if let Some(browser) = find_app_browser(&browser_candidates(), |p| p.is_file())
        && spawn_quiet(Command::new(browser).args(app_window_args(url))).is_ok()
    {
        return;
    }
    shell_open(url);
}

/// 用系统默认程序打开（网址、文件夹）。
#[cfg(windows)]
pub fn shell_open(target: &str) {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let wide = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let (verb, file) = (wide("open"), wide(target));
    // SAFETY: 所有字符串都是以 0 结尾的 UTF-16，调用期间有效；其余参数按文档可以为空。
    unsafe {
        ShellExecuteW(std::ptr::null_mut(), verb.as_ptr(), file.as_ptr(), std::ptr::null(), std::ptr::null(), SW_SHOWNORMAL);
    }
}

#[cfg(not(windows))]
pub fn shell_open(target: &str) {
    let _ = spawn_quiet(Command::new("xdg-open").arg(target));
}

/// 在资源管理器里选中这个文件。
pub fn reveal(path: &Path) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = spawn_quiet(Command::new(explorer_exe()).raw_arg(explorer_select_arg(path)));
    }
    #[cfg(not(windows))]
    shell_open(&path.parent().unwrap_or(path).to_string_lossy());
}

pub fn open_folder(path: &Path) {
    #[cfg(windows)]
    {
        let _ = spawn_quiet(Command::new(explorer_exe()).arg(display_path(path)));
    }
    #[cfg(not(windows))]
    shell_open(&path.to_string_lossy());
}

/// 没有控制台时告诉用户出了什么事。
pub fn show_error(message: &str) {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MessageBoxW};
        let wide = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
        let (text, title) = (wide(message), wide("局域网快传"));
        // SAFETY: 两个字符串都以 0 结尾、调用期间有效；父窗口为空表示没有所属窗口。
        unsafe { MessageBoxW(std::ptr::null_mut(), text.as_ptr(), title.as_ptr(), MB_ICONERROR) };
    }
    eprintln!("{message}");
}

/// 从终端启动时接上父进程的控制台，`--help`、`--no-tray` 才能看到输出。
/// 返回“标准输出能不能用”：接上了控制台，或者输出被重定向到了管道/文件（自动化测试）都算能用；
/// 双击启动时两者都没有，返回 false，出错时改用弹窗。
pub fn attach_parent_console() -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
        use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_OUTPUT_HANDLE};
        // SAFETY: 只是请求接上父进程的控制台，失败（没有父控制台）时返回 0，不影响已有的标准句柄。
        unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
        // SAFETY: 只读取本进程的标准输出句柄。
        let out = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
        !out.is_null() && out != INVALID_HANDLE_VALUE
    }
    #[cfg(not(windows))]
    true
}

/// `server::Desktop` 的真实实现。
pub struct Explorer;

impl crate::server::Desktop for Explorer {
    fn reveal(&self, path: &Path) {
        reveal(path);
    }
    fn open_folder(&self, path: &Path) {
        open_folder(path);
    }
    fn open_app(&self, url: &str) {
        open_app_window(url);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_verbatim_prefixes() {
        assert_eq!(display_path(Path::new(r"\\?\C:\Users\a\Downloads\LanShare\x.mp4")), PathBuf::from(r"C:\Users\a\Downloads\LanShare\x.mp4"));
        assert_eq!(display_path(Path::new(r"\\?\UNC\nas\share\x.mp4")), PathBuf::from(r"\\nas\share\x.mp4"));
        assert_eq!(display_path(Path::new(r"C:\plain\x.mp4")), PathBuf::from(r"C:\plain\x.mp4"));
    }

    #[test]
    fn explorer_argument_keeps_spaces_and_commas_inside_quotes() {
        let arg = explorer_select_arg(Path::new(r"\\?\C:\共享 文件夹\报价, 终版.pdf"));
        assert_eq!(arg, r#"/select,"C:\共享 文件夹\报价, 终版.pdf""#);
    }

    #[test]
    fn prefers_edge_then_chrome() {
        let c = vec![PathBuf::from("x/msedge.exe"), PathBuf::from("y/msedge.exe"), PathBuf::from("z/chrome.exe")];
        assert_eq!(find_app_browser(&c, |p| p.starts_with("y") || p.starts_with("z")), Some(PathBuf::from("y/msedge.exe")));
        assert_eq!(find_app_browser(&c, |_| false), None);
        let defaults = browser_candidates();
        if let Some(first_chrome) = defaults.iter().position(|p| p.ends_with("chrome.exe")) {
            assert!(defaults[..first_chrome].iter().all(|p| p.ends_with("msedge.exe")), "Edge 全部排在 Chrome 前面");
        }
    }

    #[test]
    fn app_window_has_no_address_bar_flag() {
        let args = app_window_args("http://127.0.0.1:8000/#abc");
        assert_eq!(args[0], "--app=http://127.0.0.1:8000/#abc");
    }

    #[test]
    fn explorer_is_an_absolute_path() {
        assert!(explorer_exe().is_absolute());
        assert!(explorer_exe().ends_with("explorer.exe"));
    }
}
