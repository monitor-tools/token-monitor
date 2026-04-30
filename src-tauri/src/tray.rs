//! 系统托盘模块
//!
//! 菜单项行为：
//! - 控制面板：可见时隐藏，隐藏时显示（切换）
//! - 悬浮窗：可见时隐藏，隐藏时显示（切换）
//! - 退出：关闭整个应用

use tauri::{
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    AppHandle, Manager, Wry,
};

#[cfg(target_os = "macos")]
use tauri::Listener;

pub fn setup_tray(handle: &AppHandle) -> tauri::Result<()> {
    let menu = build_menu(handle)?;

    let tray_builder = TrayIconBuilder::new()
        .icon(handle.default_window_icon().unwrap().clone())
        .menu(&menu);
    
    // macOS: 左键点击打开悬浮窗
    #[cfg(target_os = "macos")]
    let tray_builder = tray_builder.show_menu_on_left_click(false);
    
    // 其他平台: 保持默认行为
    #[cfg(not(target_os = "macos"))]
    let tray_builder = tray_builder.show_menu_on_left_click(false);
    
    let tray = tray_builder.build(handle)?;

    // 保存 tray 的引用以便后续更新
    let app_handle = handle.clone();
    let tray_clone = tray.clone();
    
    tray.on_menu_event(move |app, event| {
        match event.id.as_ref() {
            "toggle_control" => {
                toggle_window(app, "control");
                // 切换后更新菜单
                update_tray_menu(&app_handle, &tray_clone);
            }
            "toggle_overlay" => {
                toggle_window(app, "overlay");
                // 切换后更新菜单
                update_tray_menu(&app_handle, &tray_clone);
            }
            "quit" => app.exit(0),
            _ => {}
        }
    });

    // macOS: 左键点击托盘图标切换悬浮窗显示/隐藏
    #[cfg(target_os = "macos")]
    {
        use std::sync::{Arc, Mutex};
        use std::time::{Duration, Instant};
        
        let app_click = handle.clone();
        let last_click = Arc::new(Mutex::new(Instant::now() - Duration::from_secs(1)));
        
        tray.on_tray_icon_event(move |_tray, event| {
            // 只响应 Click 事件（完整的点击，不是 MouseDown/MouseUp）
            if let tauri::tray::TrayIconEvent::Click { 
                button, 
                button_state,
                ..
            } = event {
                // 只在鼠标释放时处理（避免按下和释放都触发）
                if button == tauri::tray::MouseButton::Left 
                    && button_state == tauri::tray::MouseButtonState::Up {
                    
                    // 防抖：如果距离上次点击少于 300ms，忽略
                    let mut last = last_click.lock().unwrap();
                    let now = Instant::now();
                    if now.duration_since(*last) < Duration::from_millis(300) {
                        eprintln!("[tray] Click ignored (debounce)");
                        return;
                    }
                    *last = now;
                    drop(last);
                    
                    eprintln!("[tray] Left click detected on macOS");
                    let app = app_click.clone();
                    let _ = app_click.run_on_main_thread(move || {
                        if let Some(overlay) = app.get_webview_window("overlay") {
                            let is_visible = overlay.is_visible().unwrap_or(false);
                            eprintln!("[tray] Overlay is_visible: {}", is_visible);
                            
                            if is_visible {
                                eprintln!("[tray] Hiding overlay");
                                let _ = overlay.hide();
                            } else {
                                eprintln!("[tray] Showing overlay");
                                let _ = overlay.show();
                                let _ = overlay.set_focus();
                            }
                        } else {
                            eprintln!("[tray] Overlay window not found");
                        }
                    });
                }
            }
        });
    }

    // 仅在 macOS 下监听数据更新事件以更新托盘标题
    #[cfg(target_os = "macos")]
    {
        let tray_data = tray.clone();
        handle.listen("provider_data_updated", move |event| {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(event.payload()) {
                update_tray_title(&tray_data, &json);
            }
        });
    }

    Ok(())
}

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

    let control_text = if control_visible {
        "控制面板 ●"
    } else {
        "控制面板 ○"
    };
    
    let overlay_text = if overlay_visible {
        "悬浮窗 ●"
    } else {
        "悬浮窗 ○"
    };

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

/// 更新托盘标题（仅 macOS）
/// 从 provider_data_updated 事件中提取最近5小时的配额信息并显示在托盘标题
#[cfg(target_os = "macos")]
fn update_tray_title(tray: &tauri::tray::TrayIcon<Wry>, data: &serde_json::Value) {
    // 查找 quota_groups 中的第一个配额组（通常是"近5小时"）
    if let Some(quota_groups) = data.get("quota_groups").and_then(|v| v.as_array()) {
        if let Some(first_quota) = quota_groups.first() {
            let remain = first_quota.get("remain").and_then(|v| v.as_u64()).unwrap_or(0);
            let limit = first_quota.get("limit").and_then(|v| v.as_u64()).unwrap_or(0);
            
            // 如果有限额，显示剩余量
            if limit > 0 {
                // 格式化剩余量，如果超过 10000 则显示为 k 单位
                let remain_str = if remain >= 10000 {
                    format!("{}k", remain / 1000)
                } else {
                    remain.to_string()
                };
                
                // 将数字转换为下标形式
                let subscript_str = to_subscript(&remain_str);
                
                // 图标和数字之间留一个空格
                let title = format!(" {}", subscript_str);
                let _ = tray.set_title(Some(&title));
                eprintln!("[tray] Updated title to: {} (subscript)", remain_str);
                return;
            }
        }
    }
    
    // 如果没有配额数据，清空标题
    let _ = tray.set_title(Option::<&str>::None);
    eprintln!("[tray] Cleared title (no quota data)");
}

/// 将数字字符串转换为 Unicode 下标数字
#[cfg(target_os = "macos")]
fn to_subscript(s: &str) -> String {
    let mut result = String::new();
    for c in s.chars() {
        match c {
            '0' => result.push('₀'),
            '1' => result.push('₁'),
            '2' => result.push('₂'),
            '3' => result.push('₃'),
            '4' => result.push('₄'),
            '5' => result.push('₅'),
            '6' => result.push('₆'),
            '7' => result.push('₇'),
            '8' => result.push('₈'),
            '9' => result.push('₉'),
            'k' => result.push('ₖ'), // 下标 k
            _ => result.push(c), // 保持其他字符不变
        }
    }
    result
}
