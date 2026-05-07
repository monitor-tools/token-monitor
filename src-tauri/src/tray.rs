//! 系统托盘模块
//!
//! 菜单项行为：
//! - 控制面板：可见时隐藏，隐藏时显示（切换）
//! - 悬浮窗：可见时隐藏，隐藏时显示（切换）
//! - 退出：关闭整个应用
//!
//! macOS 专属功能（托盘标题更新、左键点击事件）委托给 `macos_tray` 模块。

use tauri::{
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    AppHandle, Manager, Wry,
};

pub fn setup_tray(handle: &AppHandle) -> tauri::Result<()> {
    let menu = build_menu(handle)?;

    let tray = TrayIconBuilder::new()
        .icon(handle.default_window_icon().unwrap().clone())
        .menu(&menu)
        .show_menu_on_left_click(false)
        .build(handle)?;

    // macOS：保存托盘引用 + 注册左键点击事件
    #[cfg(target_os = "macos")]
    {
        use crate::macos_tray;
        macos_tray::setup_click_handler(handle, &tray);
        macos_tray::register_tray(tray.clone());
    }

    let app_handle = handle.clone();
    let tray_clone = tray.clone();

    tray.on_menu_event(move |app, event| {
        match event.id.as_ref() {
            "toggle_control" => {
                let app_clone = app.clone();
                let tray_update = tray_clone.clone();
                let handle_update = app_handle.clone();

                let _ = app.run_on_main_thread(move || {
                    toggle_window(&app_clone, "control");
                    std::thread::spawn(move || {
                        std::thread::sleep(std::time::Duration::from_millis(100));
                        update_tray_menu(&handle_update, &tray_update);
                    });
                });
            }
            "toggle_overlay" => {
                let app_clone = app.clone();
                let tray_update = tray_clone.clone();
                let handle_update = app_handle.clone();

                let _ = app.run_on_main_thread(move || {
                    toggle_window_with_recovery(&app_clone, "overlay");
                    std::thread::spawn(move || {
                        std::thread::sleep(std::time::Duration::from_millis(100));
                        update_tray_menu(&handle_update, &tray_update);
                    });
                });
            }
            "quit" => app.exit(0),
            _ => {}
        }
    });

    Ok(())
}

// ─── 私有实现 ─────────────────────────────────────────────────────────────────

/// 更新托盘菜单
fn update_tray_menu(handle: &AppHandle, tray: &tauri::tray::TrayIcon<Wry>) {
    if let Ok(new_menu) = build_menu(handle) {
        let _ = tray.set_menu(Some(new_menu));
    }
}

/// 构建托盘菜单，根据窗口状态显示不同的文本
fn build_menu(handle: &AppHandle) -> tauri::Result<Menu<Wry>> {
    let control_visible = handle
        .get_webview_window("control")
        .and_then(|w| w.is_visible().ok())
        .unwrap_or(false);

    let overlay_visible = handle
        .get_webview_window("overlay")
        .and_then(|w| w.is_visible().ok())
        .unwrap_or(false);

    let control_text = if control_visible { "控制面板 ●" } else { "控制面板 ○" };
    let overlay_text = if overlay_visible { "悬浮窗 ●"   } else { "悬浮窗 ○"   };

    let control_item = MenuItem::with_id(handle, "toggle_control", control_text, true, None::<&str>)?;
    let overlay_item = MenuItem::with_id(handle, "toggle_overlay", overlay_text, true, None::<&str>)?;
    let quit_item    = MenuItem::with_id(handle, "quit",           "退出",        true, None::<&str>)?;

    Menu::with_items(handle, &[&control_item, &overlay_item, &quit_item])
}

/// 切换指定窗口的可见状态：可见则隐藏，隐藏则显示并聚焦。
fn toggle_window(app: &AppHandle, label: &str) {
    if let Some(w) = app.get_webview_window(label) {
        if w.is_visible().unwrap_or(false) {
            let _ = w.hide();
        } else {
            let _ = w.show();
            let _ = w.set_focus();
        }
    }
}

/// 切换 overlay 窗口的可见状态，带恢复机制（Windows 专用）
fn toggle_window_with_recovery(app: &AppHandle, label: &str) {
    if let Some(w) = app.get_webview_window(label) {
        let is_visible = w.is_visible().unwrap_or(false);

        if is_visible {
            let _ = w.hide();
        } else {
            #[cfg(target_os = "windows")]
            {
                let _ = w.set_skip_taskbar(false);
                let _ = w.set_always_on_top(true);
            }

            let _ = w.show();
            let _ = w.set_focus();

            #[cfg(target_os = "windows")]
            {
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    let _ = w.set_focus();
                });
            }
        }
    }
}
