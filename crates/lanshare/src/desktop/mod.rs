//! 桌面外壳：单实例、托盘、应用窗口、资源管理器、弹窗。

pub mod instance;
pub mod shell;
#[cfg(windows)]
pub mod tray;
