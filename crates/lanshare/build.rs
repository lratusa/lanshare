//! 给 exe 嵌入图标和版本信息（资源管理器、任务栏、托盘都用这个图标）。

fn main() {
    println!("cargo:rerun-if-changed=web/icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("web/icon.ico"); // 资源编号 1，托盘图标从这里读
        res.set("FileDescription", "LanShare - LAN file transfer (end-to-end encrypted)");
        res.set("ProductName", "LanShare");
        res.set("OriginalFilename", "LanShare.exe");
        if let Err(e) = res.compile() {
            println!("cargo:warning=嵌入图标失败：{e}");
        }
    }
}
