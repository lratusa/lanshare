// 双击运行时不弹黑色命令行窗口；从终端运行时用 AttachConsole 接上父进程的控制台。
#![cfg_attr(windows, windows_subsystem = "windows")]

//! 程序入口：命令行参数 → 单实例 → 绑定端口 → 启动服务 → 打开主界面 → 托盘（或前台运行）。

use std::net::Ipv4Addr;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lanshare::cli::{self, Args, CliError};
use lanshare::desktop::instance::{self, InstanceLock, InstanceRecord};
use lanshare::desktop::shell;
use lanshare::logging::FileLog;
use lanshare::net;
use lanshare::netinfo;
use lanshare::server::{self, AppState, Config};

const MUTEX_NAME: &str = "Local\\LanShare-v2";

/// 有控制台就打印，没有就弹窗。
fn report(has_console: bool, message: &str) {
    if has_console {
        eprintln!("{message}");
    } else {
        shell::show_error(message);
    }
}

fn main() -> ExitCode {
    let has_console = shell::attach_parent_console();
    let args = match cli::parse(std::env::args().skip(1), cli::default_share_dir()) {
        Ok(args) => args,
        Err(CliError::Help) => {
            report(has_console, cli::HELP);
            return ExitCode::SUCCESS;
        }
        Err(CliError::Invalid(msg)) => {
            report(has_console, &format!("{msg}\n\n{}", cli::HELP));
            return ExitCode::from(2);
        }
    };
    let data_dir = cli::data_dir();
    let log = Arc::new(FileLog::new(data_dir.join("lanshare.log"), has_console));
    let record_path = data_dir.join("instance.json");

    // 单实例：拿不到互斥量说明已经有一个在运行，把主界面交给它打开
    let _lock = if args.single_instance {
        match InstanceLock::acquire(MUTEX_NAME) {
            Some(lock) => {
                // 上次异常退出（崩溃、关机）可能留下旧记录：拿到锁后第一件事就删掉，
                // 免得后启动的实例读到旧端口、旧密钥
                let _ = std::fs::remove_file(&record_path);
                Some(lock)
            }
            None => return hand_over_to_running(&record_path, &args, &log, has_console),
        }
    } else {
        None
    };

    match run(&args, &log, &record_path) {
        Ok(()) => {
            log.write("已退出");
            ExitCode::SUCCESS
        }
        Err(message) => {
            log.write(&message);
            // --no-tray 是给终端和自动化测试用的：不弹窗，免得卡住测试
            report(has_console || args.no_tray, &message);
            ExitCode::from(1)
        }
    }
}

fn run(args: &Args, log: &Arc<FileLog>, record_path: &Path) -> Result<(), String> {
    let listener = net::bind_with_fallback(Ipv4Addr::UNSPECIFIED, args.port).map_err(|e| {
        format!("启动失败：从 {} 开始的 {} 个端口都用不了（{e}）。\n请关掉占用这些端口的程序后再试。", args.port, net::PORT_TRIES)
    })?;
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(args.port);

    let mut config = Config::new(args.dir.clone());
    config.lan_ips = netinfo::lan_ips();
    config.desktop = Some(Arc::new(shell::Explorer));
    let sink = log.clone();
    config.log = Arc::new(move |line: &str| sink.write(line));
    let state = Arc::new(AppState::new(config, port));

    let removed = state.cleanup_stale_temp();
    if removed > 0 {
        log.write(&format!("清理了 {removed} 个上次没传完的临时文件"));
    }
    if args.single_instance {
        let record = InstanceRecord { port, key: state.key_b64(), pid: std::process::id() };
        instance::write_record(record_path, &record).map_err(|e| format!("写实例记录失败：{e}"))?;
    }
    if let Some(path) = &args.info_file {
        let info = serde_json::json!({
            "port": port,
            "pin": state.pin(),
            "key": state.key_b64(),
            "server_id": state.server_id(),
            "urls": state.base_urls(),
        });
        std::fs::write(path, info.to_string()).map_err(|e| format!("写 info 文件失败：{e}"))?;
    }
    log.write(&format!(
        "局域网快传 v{} 已启动：{}（共享文件夹 {}）",
        env!("CARGO_PKG_VERSION"),
        state.base_urls()[0],
        state.share_dir().display()
    ));

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("创建运行时失败：{e}"))?;
    let listener = {
        let _guard = runtime.enter(); // from_std 必须在运行时里调用
        tokio::net::TcpListener::from_std(listener).map_err(|e| format!("监听失败：{e}"))?
    };
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let server_task = runtime.spawn(server::serve_with_shutdown(listener, state.clone(), async {
        let _ = stop_rx.await;
    }));

    if !args.no_browser {
        shell::open_app_window(&state.local_entry_url());
    }

    let result = if args.no_tray {
        // 前台运行：Ctrl+C 或服务出错时退出
        runtime.block_on(async {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => Ok(()),
                r = server_task => match r {
                    Ok(Ok(())) => Ok(()),
                    Ok(Err(e)) => Err(format!("服务出错：{e}")),
                    Err(e) => Err(format!("服务异常退出：{e}")),
                },
            }
        })
    } else {
        let tray = run_tray(&state);
        let _ = stop_tx.send(());
        runtime.block_on(async {
            let _ = tokio::time::timeout(Duration::from_secs(3), server_task).await;
        });
        tray
    };
    if args.single_instance {
        instance::remove_record_if_own(record_path, std::process::id());
    }
    result
}

#[cfg(windows)]
fn run_tray(state: &Arc<AppState>) -> Result<(), String> {
    let url = state.local_entry_url();
    let folder = state.share_dir().to_path_buf();
    lanshare::desktop::tray::run(
        "局域网快传（端到端加密）",
        Arc::new(move || shell::open_app_window(&url)),
        Arc::new(move || shell::open_folder(&folder)),
    )
    .map_err(|e| format!("托盘图标创建失败：{e}"))
}

#[cfg(not(windows))]
fn run_tray(_state: &Arc<AppState>) -> Result<(), String> {
    Err("托盘只在 Windows 上可用，请加 --no-tray".to_string())
}

/// 已经有实例在运行：等它写好实例记录（它可能刚启动），然后打开它的主界面。
fn hand_over_to_running(record_path: &Path, args: &Args, log: &FileLog, has_console: bool) -> ExitCode {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Some(record) = instance::read_record(record_path) {
            if !args.no_browser {
                shell::open_app_window(&format!("http://127.0.0.1:{}/#{}", record.port, record.key));
            }
            log.write("已经在运行，打开已有实例的主界面");
            return ExitCode::SUCCESS;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    report(has_console, "局域网快传已经在运行，但没有响应。\n可以在任务管理器里结束 LanShare.exe 后再试。");
    ExitCode::from(1)
}
