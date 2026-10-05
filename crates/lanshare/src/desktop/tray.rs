//! 托盘图标与 Win32 消息循环（必须在创建图标的那个线程上跑，这里就是主线程）。
//!
//! 菜单：打开局域网快传（左键单击图标也是它）/ 打开共享文件夹 / 退出。
//! tray-icon 的隐藏窗口把不认识的消息交给 DefWindowProcW，所以会同意关机（v1 的 pystray 会拒绝）。

use std::sync::Arc;

use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, MSG, PostQuitMessage, TranslateMessage,
};

pub type Action = Arc<dyn Fn() + Send + Sync>;

/// exe 里图标资源的编号（build.rs 用 winresource 嵌入，编号 1）。
const ICON_RESOURCE: u16 = 1;

/// 显示托盘图标并阻塞，直到用户选“退出”（或者系统要求结束会话）。
pub fn run(tooltip: &str, on_open: Action, on_folder: Action) -> Result<(), String> {
    let menu = Menu::new();
    let open = MenuItem::with_id("open", "打开局域网快传", true, None);
    let folder = MenuItem::with_id("folder", "打开共享文件夹", true, None);
    let quit = MenuItem::with_id("quit", "退出", true, None);
    menu.append_items(&[&open, &folder, &PredefinedMenuItem::separator(), &quit])
        .map_err(|e| e.to_string())?;

    let icon = Icon::from_resource(ICON_RESOURCE, Some((32, 32))).map_err(|e| e.to_string())?;
    let _tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false) // 左键直接打开主界面，右键才弹菜单
        .with_tooltip(tooltip)
        .with_icon(icon)
        .build()
        .map_err(|e| e.to_string())?;

    let open2 = on_open.clone();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| match event.id().as_ref() {
        "open" => open2(),
        "folder" => on_folder(),
        // SAFETY: 回调在 DispatchMessageW 里、也就是消息循环所在的主线程上被调用。
        "quit" => unsafe { PostQuitMessage(0) },
        _ => {}
    }));
    TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
        if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
            on_open();
        }
    }));

    // SAFETY: 标准的 Win32 消息循环；msg 是本地变量，GetMessageW 返回 0 表示收到 WM_QUIT。
    unsafe {
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    MenuEvent::set_event_handler(None::<fn(MenuEvent)>);
    TrayIconEvent::set_event_handler(None::<fn(TrayIconEvent)>);
    Ok(())
}
