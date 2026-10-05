//! 命令行参数：`LanShare.exe [--port 8000] [--dir 共享文件夹] [--no-browser] [--no-tray] [--info-file 路径]`
//!
//! 没用 clap：只有 5 个参数，手写解析更小，也更好讲解。

use std::path::PathBuf;

pub const DEFAULT_PORT: u16 = 8000;

pub const HELP: &str = "局域网快传 LanShare

用法：LanShare.exe [选项]
  --port <端口>      起始端口，被占用时自动往后找（默认 8000）
  --dir <文件夹>     共享文件夹（默认：下载\\LanShare）
  --no-browser       启动后不打开主界面
  --no-tray          不用托盘，前台运行（Ctrl+C 退出）
  --info-file <路径> 启动后把端口、口令、密钥写进这个 JSON 文件（给自动化测试用）
  --help             显示这段说明";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Args {
    pub port: u16,
    pub dir: PathBuf,
    pub no_browser: bool,
    pub no_tray: bool,
    pub info_file: Option<PathBuf>,
    /// 只有默认启动（双击、开始菜单、任务栏）才保持单实例；显式指定端口或目录时照常另起一个。
    pub single_instance: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum CliError {
    Help,
    Invalid(String),
}

/// 默认共享文件夹：系统“下载”文件夹下的 LanShare（下载文件夹被挪到别的盘也能找到）。
pub fn default_share_dir() -> PathBuf {
    dirs::download_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join("Downloads")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("LanShare")
}

/// 程序数据目录（日志、单实例记录）：`%LOCALAPPDATA%\LanShare`。
/// 先看环境变量：测试会把它指到临时目录，避免碰本机真实数据。
pub fn data_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(dirs::data_local_dir)
        .unwrap_or_else(std::env::temp_dir)
        .join("LanShare")
}

pub fn parse(args: impl IntoIterator<Item = String>, default_dir: PathBuf) -> Result<Args, CliError> {
    let mut port: Option<u16> = None;
    let mut dir: Option<PathBuf> = None;
    let mut no_browser = false;
    let mut no_tray = false;
    let mut info_file = None;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().ok_or_else(|| CliError::Invalid(format!("{name} 后面缺少值")));
        match arg.as_str() {
            "--port" => {
                let v = value("--port")?;
                port = Some(v.parse().map_err(|_| CliError::Invalid(format!("端口不对：{v}")))?);
            }
            "--dir" => dir = Some(PathBuf::from(value("--dir")?)),
            "--info-file" => info_file = Some(PathBuf::from(value("--info-file")?)),
            "--no-browser" => no_browser = true,
            "--no-tray" => no_tray = true,
            "--help" | "-h" | "/?" => return Err(CliError::Help),
            other => return Err(CliError::Invalid(format!("不认识的参数：{other}"))),
        }
    }
    Ok(Args {
        single_instance: port.is_none() && dir.is_none(),
        port: port.unwrap_or(DEFAULT_PORT),
        dir: dir.unwrap_or(default_dir),
        no_browser,
        no_tray,
        info_file,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(list: &[&str]) -> Result<Args, CliError> {
        parse(list.iter().map(|s| s.to_string()), PathBuf::from("D:/default"))
    }

    #[test]
    fn defaults_mean_single_instance() {
        let a = p(&[]).unwrap();
        assert_eq!((a.port, a.dir.clone(), a.single_instance), (8000, PathBuf::from("D:/default"), true));
        assert!(!a.no_browser && !a.no_tray && a.info_file.is_none());
        assert!(p(&["--no-browser", "--no-tray"]).unwrap().single_instance);
    }

    #[test]
    fn explicit_port_or_dir_disables_single_instance() {
        assert!(!p(&["--port", "9000"]).unwrap().single_instance);
        assert!(!p(&["--dir", "E:/x"]).unwrap().single_instance);
        let a = p(&["--port", "0", "--dir", "E:/x", "--info-file", "E:/i.json"]).unwrap();
        assert_eq!((a.port, a.dir, a.info_file), (0, PathBuf::from("E:/x"), Some(PathBuf::from("E:/i.json"))));
    }

    #[test]
    fn errors() {
        assert_eq!(p(&["--help"]), Err(CliError::Help));
        assert!(matches!(p(&["--port"]), Err(CliError::Invalid(_))));
        assert!(matches!(p(&["--port", "70000"]), Err(CliError::Invalid(_))), "超出 u16 范围");
        assert!(matches!(p(&["--port", "abc"]), Err(CliError::Invalid(_))));
        assert!(matches!(p(&["--bogus"]), Err(CliError::Invalid(_))));
    }

    #[test]
    fn data_dir_follows_localappdata() {
        // 只读检查：不改环境变量（测试是并行跑的）
        let expected = std::env::var_os("LOCALAPPDATA").map(|v| PathBuf::from(v).join("LanShare"));
        if let Some(expected) = expected {
            assert_eq!(data_dir(), expected);
        }
        assert!(data_dir().ends_with("LanShare"));
    }

    #[test]
    fn default_dir_ends_with_lanshare() {
        assert!(default_share_dir().ends_with("LanShare"));
    }
}
