// Windows Release 构建：禁止弹出额外的控制台窗口
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::collections::HashMap;
use std::sync::Mutex;

mod overlay;
mod providers;
#[cfg(target_os = "windows")]
mod windows_taskbar;
#[cfg(target_os = "macos")]
mod macos_tray;
mod tray;
mod window;

use tauri::{AppHandle, Listener, Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent};

/// Rust 侧 Provider 状态追踪（"connecting" | "connected" | "disconnected"）
struct AppState {
    provider_status: Mutex<HashMap<String, String>>,
}

fn main() {
    // macOS: 设置为辅助应用，不在 Dock 中显示图标
    #[cfg(target_os = "macos")]
    {
        use tauri::ActivationPolicy;
        tauri::Builder::default()
            .plugin(tauri_plugin_shell::init())
            .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
                // 已有实例运行时，优先显示控制面板
                if let Some(w) = app.get_webview_window("control") {
                    let _ = w.show();
                    let _ = w.set_focus();
                }
            }))
            .setup(|app| {
                // 设置为辅助应用（不显示 Dock 图标）
                app.set_activation_policy(ActivationPolicy::Accessory);

                let handle = app.handle().clone();
                app.manage(AppState { provider_status: Mutex::new(HashMap::new()) });

                tray::setup_tray(&handle)?;

                // 创建共享 Overlay 窗口（初始隐藏）
                let overlay_win = overlay::create_overlay(&handle)?;
                overlay::setup_overlay_events(&overlay_win);

                // 全局事件监听
                setup_global_events(&overlay_win, &handle);

                // 创建并显示控制面板窗口
                window::create_control_panel(&handle)?;

                Ok(())
            })
            .invoke_handler(tauri::generate_handler![
                connect_provider,
                hide_control_panel,
                set_refresh_interval,
                set_taskbar_widget_colors,
            ])
            .run(tauri::generate_context!())
            .expect("error while running tauri application");
    }

    // 其他平台：正常显示
    #[cfg(not(target_os = "macos"))]
    {
        tauri::Builder::default()
            .plugin(tauri_plugin_shell::init())
            .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
                // 已有实例运行时，优先显示控制面板
                if let Some(w) = app.get_webview_window("control") {
                    let _ = w.show();
                    let _ = w.set_focus();
                }
            }))
            .setup(|app| {
                let handle = app.handle().clone();
                app.manage(AppState { provider_status: Mutex::new(HashMap::new()) });

                tray::setup_tray(&handle)?;

                // 创建共享 Overlay 窗口（初始隐藏）
                let overlay_win = overlay::create_overlay(&handle)?;
                overlay::setup_overlay_events(&overlay_win);

                // Windows: 创建任务栏文字组件（原生窗口）
                #[cfg(target_os = "windows")]
                {
                    if let Ok(_) = windows_taskbar::create_taskbar_widget(&handle) {
                        eprintln!("[main] 任务栏组件创建成功");
                        windows_taskbar::setup_taskbar_widget_events(&handle);
                    } else {
                        eprintln!("[main] 任务栏组件创建失败");
                    }
                }

                // 全局事件监听
                setup_global_events(&overlay_win, &handle);

                // 创建并显示控制面板窗口
                window::create_control_panel(&handle)?;

                Ok(())
            })
            .invoke_handler(tauri::generate_handler![
                connect_provider,
                hide_control_panel,
                set_refresh_interval,
                set_taskbar_widget_colors,
            ])
            .run(tauri::generate_context!())
            .expect("error while running tauri application");
    }
}

// ─── Tauri 命令 ───────────────────────────────────────────────────────────────

/// 连接指定 Provider：创建（或重新显示）其 WebView 窗口。
///
/// 由控制面板 JS 调用：`invoke('connect_provider', { providerId: 'aliyun' })`
#[tauri::command]
async fn connect_provider(app: AppHandle, provider_id: String, refresh_interval: Option<u32>) -> Result<(), String> {
    println!("[DEBUG] connect_provider 被调用: provider_id={}, refresh_interval={:?}", provider_id, refresh_interval);

    // 在 Windows 上必须使用 async 命令来创建窗口，否则会挂起
    // 参考: https://github.com/tauri-apps/tauri/issues/4121
    create_provider_window_internal(&app, &provider_id, refresh_interval)?;

    Ok(())
}

fn create_provider_window_internal(app: &AppHandle, provider_id: &str, refresh_interval: Option<u32>) -> Result<(), String> {
    let all = providers::all_providers();
    let provider = all
        .into_iter()
        .find(|p| p.id == provider_id)
        .ok_or_else(|| {
            let err = format!("未知 Provider: {}", provider_id);
            eprintln!("[ERROR] {}", err);
            err
        })?;

    println!("[DEBUG] 找到 Provider: name={}, target_url={}", provider.name, provider.target_url);
    let label = provider.window_label();
    println!("[DEBUG] 窗口 label: {}", label);

    if let Some(win) = app.get_webview_window(&label) {
        // 窗口已存在：根据当前状态决定行为
        let current_status = {
            let state = app.state::<AppState>();
            let map = state.provider_status.lock().unwrap();
            map.get(provider_id).cloned().unwrap_or_default()
        };

        if current_status == "connected" {
            // 已连接状态（用户点击"重新登录"）：仅显示窗口，不导航，不更改状态
            // 登录检测继续在后台运行，但 Rust 侧会忽略已连接状态下的 login_detected 事件
            println!("[DEBUG] 已连接状态（重新登录模式），仅显示窗口");
        } else {
            // 非已连接状态（Session 过期重连）：导航到目标页并通知"连接中"
            println!("[DEBUG] 非已连接状态，导航到目标页");
            let _ = win.eval(&format!("window.location.href = '{}'", provider.target_url));
            notify_control(app, provider_id, "connecting");
        }

        let _ = win.show();
        let _ = win.set_focus();
        return Ok(());
    } else {
        // 首次连接：创建新的 Provider WebView 窗口
        println!("[DEBUG] 开始创建新窗口...");
        println!("[DEBUG] 配置信息:");
        println!("  - title: {}", provider.name);
        println!("  - inner_size: 1280x800");
        println!("  - min_inner_size: 800x600");
        println!("  - center: true");
        println!("  - on_navigation: 允许所有导航");
        println!("  - injection_script 长度: {} 字符", provider.injection_script.len());

        let url: tauri::Url = provider
            .target_url
            .parse()
            .map_err(|e| {
                let err = format!("URL 解析失败: {}", e);
                eprintln!("[ERROR] {}", err);
                err
            })?;

        println!("[DEBUG] URL 解析成功: {:?}", url);

        println!("[DEBUG] 开始创建窗口...");
        println!("[DEBUG] 目标 URL: {}", provider.target_url);

        // 获取主显示器尺寸，确保窗口不超过屏幕大小
        let (window_width, window_height) = if let Some(monitor) = app.primary_monitor().ok().flatten() {
            let size = monitor.size();
            let screen_width = size.width as f64;
            let screen_height = size.height as f64;

            // 期望窗口大小
            let desired_width = 1280.0_f64;
            let desired_height = 800.0_f64;

            // 留出一些边距（屏幕的 90%）
            let max_width = screen_width * 0.9;
            let max_height = screen_height * 0.9;

            let final_width = if desired_width < max_width { desired_width } else { max_width };
            let final_height = if desired_height < max_height { desired_height } else { max_height };

            println!("[DEBUG] 屏幕尺寸: {}x{}, 窗口尺寸: {}x{}",
                     screen_width, screen_height, final_width, final_height);

            (final_width, final_height)
        } else {
            println!("[WARN] 无法获取屏幕尺寸，使用默认值");
            (1280.0, 800.0)
        };

        let win = WebviewWindowBuilder::new(
            app,
            label.clone(),
            WebviewUrl::External(url.clone()),
        )
        .title(provider.name)
        .inner_size(window_width, window_height)
        .center()
        .visible(false) // 初始隐藏
        // initialization_script 在每次页面加载（含导航跳转）时自动注入脚本，
        // 避免依赖 eval 一次性注入：用户在登录页完成登录后跳回控制台时脚本仍能运行。
        .initialization_script(&provider.injection_script)
        .build()
        .map_err(|e| {
            let err = format!("窗口创建失败: {}", e);
            eprintln!("[ERROR] {}", err);
            err
        })?;

        println!("[DEBUG] ✓ 窗口创建成功（initialization_script 已注册）: {}", label);

        // 如果提供了刷新间隔配置，立即应用
        if let Some(interval_sec) = refresh_interval {
            let interval_ms = interval_sec * 1000;
            let script = format!("if (window.__LSYS_SET_INTERVAL__) window.__LSYS_SET_INTERVAL__({});", interval_ms);
            let win_interval = win.clone();
            let handle_interval = app.clone();
            handle_interval.run_on_main_thread(move || {
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(1000));
                    let _ = win_interval.eval(&script);
                    println!("[DEBUG] 已应用刷新间隔配置: {}秒", interval_sec);
                });
            }).ok();
        }

        // 延迟显示窗口，给页面一些加载时间
        let win_show = win.clone();
        let handle_show = app.clone();
        handle_show.run_on_main_thread(move || {
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(800));
                let _ = win_show.show();
                let _ = win_show.set_focus();
                println!("[DEBUG] 窗口延迟显示完成");
            });
        }).ok();

        // 监听子窗口关闭事件：阻止真正关闭，改为隐藏（保留 WebView 会话和 Cookie）
        // 根据当前状态决定是否通知控制面板
        let handle_close = app.clone();
        let pid_close    = provider_id.to_string();
        win.on_window_event(move |event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                // 阻止系统关闭，保留 WebView 会话
                api.prevent_close();
                println!("[DEBUG] Provider 窗口关闭请求（将改为隐藏）: {}", pid_close);

                // 检查当前 Rust 侧状态决定行为
                let current_status = {
                    let state = handle_close.state::<AppState>();
                    let map = state.provider_status.lock().unwrap();
                    map.get(&pid_close).cloned().unwrap_or_default()
                };
                let is_connected = current_status == "connected";

                if !is_connected {
                    // 连接中/断开状态：通知控制面板已断开
                    notify_control(&handle_close, &pid_close, "disconnected");
                }
                // 已连接状态（重新登录模式）：保持状态不变，静默隐藏

                // UI 操作在主线程执行
                let win_label = format!("provider_{}", pid_close);
                let h = handle_close.clone();
                if let Err(e) = handle_close.run_on_main_thread(move || {
                    if let Some(w) = h.get_webview_window(&win_label) {
                        let _ = w.hide();
                    }
                    if !is_connected {
                        // 重新显示控制面板，让用户可以重新点击「连接」
                        if let Some(ctrl) = h.get_webview_window("control") {
                            let _ = ctrl.show();
                            let _ = ctrl.set_focus();
                        }
                    }
                }) {
                    eprintln!("[window] run_on_main_thread 失败: {e}");
                }
            }
        });
    }

    // 立即通知控制面板：正在连接
    notify_control(app, provider_id, "connecting");

    Ok(())
}

/// 隐藏控制面板窗口（由控制面板 JS 在所有 Provider 均已连接后调用）。
#[tauri::command]
fn hide_control_panel(app: AppHandle) {
    if let Some(w) = app.get_webview_window("control") {
        let _ = app.run_on_main_thread(move || {
            let _ = w.hide();
        });
    }
}

/// 设置刷新间隔（秒）
#[tauri::command]
fn set_refresh_interval(app: AppHandle, interval_seconds: u32) -> Result<(), String> {
    let interval_ms = interval_seconds * 1000;

    // 更新所有已打开的 Provider 窗口的刷新间隔
    for provider in providers::all_providers() {
        let label = provider.window_label();
        if let Some(win) = app.get_webview_window(&label) {
            let script = format!("if (window.__LSYS_SET_INTERVAL__) window.__LSYS_SET_INTERVAL__({});", interval_ms);
            let _ = win.eval(&script);
        }
    }

    Ok(())
}

/// 设置任务栏小组件颜色（仅 Windows，其他平台调用将被忽略）
/// 参数均为 `#RRGGBB` 格式的 HTML 颜色字符串。
#[tauri::command]
fn set_taskbar_widget_colors(
    bg_color: String,
    text_color: String,
) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    windows_taskbar::set_widget_colors(&bg_color, &text_color);
    Ok(())
}

// ─── 全局事件处理 ─────────────────────────────────────────────────────

fn setup_global_events(overlay: &tauri::WebviewWindow, handle: &AppHandle) {
    // ── provider_login_detected ─────────────────────────────────────────────────────
    let handle_clone  = handle.clone();
    let overlay_login = overlay.clone(); // 用于登录时立即推占位数据到悬浮窗
    overlay.listen("provider_login_detected", move |event| {
        println!("[DEBUG] 收到 provider_login_detected 事件");
        let Ok(json) = serde_json::from_str::<serde_json::Value>(event.payload()) else {
            eprintln!("[ERROR] provider_login_detected payload 解析失败");
            return;
        };
        let provider_id = json
            .get("provider_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        println!("[Monitor] 登录成功: provider_id={}", provider_id);

        // 检查当前状态：若已处于"connected"（重新登录模式），忽略此次事件，不隐藏窗口
        let current_status = {
            let state = handle_clone.state::<AppState>();
            let map = state.provider_status.lock().unwrap();
            map.get(provider_id).cloned().unwrap_or_default()
        };
        if current_status == "connected" {
            println!("[DEBUG] Provider 已处于 connected 状态，忽略 login_detected（重新登录模式，窗口保持打开）");
            return;
        }

        // 隐藏对应 Provider WebView（主线程安全）
        let label = format!("provider_{}", provider_id);
        println!("[DEBUG] 尝试隐藏窗口: {}", label);
        if let Some(win) = handle_clone.get_webview_window(&label) {
            let _ = handle_clone.run_on_main_thread(move || {
                if let Err(e) = win.hide() {
                    eprintln!("[ERROR] hide provider 失败: {e}");
                } else {
                    println!("[DEBUG] 窗口隐藏成功");
                }
            });
        } else {
            eprintln!("[WARN] 未找到窗口: {}", label);
        }

        // 通知控制面板：已连接
        notify_control(&handle_clone, provider_id, "connected");

        // 立即弹出悬浮窗并显示占位状态——无需等待注入脚本取到真实数据。
        // 真实数据由 JS 注入脚本异步拉取后再推送 provider_data_updated 更新。
        let provider_name = providers::all_providers()
            .into_iter()
            .find(|p| p.id == provider_id)
            .map(|p| p.name.to_string())
            .unwrap_or_else(|| provider_id.to_string());
        let init_payload = serde_json::json!({
            "provider_id":   provider_id,
            "provider_name": provider_name,
            "items": [
                {"key": "状态", "value": "已登录",         "highlight": true  },
                {"key": "数据", "value": "正在获取...", "highlight": false }
            ],
            "compact_text": "获取数据...",
            "updated_at": serde_json::Value::Null
        });
        overlay::push_provider_data(&overlay_login, &init_payload);
    });

    // ── provider_logout_detected ────────────────────────────────────────────────
    let handle_logout = handle.clone();
    overlay.listen("provider_logout_detected", move |event| {
        println!("[DEBUG] 收到 provider_logout_detected 事件");
        let Ok(json) = serde_json::from_str::<serde_json::Value>(event.payload()) else {
            eprintln!("[ERROR] provider_logout_detected payload 解析失败");
            return;
        };
        let provider_id = json
            .get("provider_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        println!("[Monitor] 退出登录: provider_id={}", provider_id);

        // 重新显示 Provider WebView，让用户重新登录
        let label = format!("provider_{}", provider_id);
        if let Some(win) = handle_logout.get_webview_window(&label) {
            let _ = handle_logout.run_on_main_thread(move || {
                let _ = win.show();
                let _ = win.set_focus();
                println!("[DEBUG] Provider 窗口已重新显示：{}", label);
            });
        }

        // 通知控制面板：已断开
        notify_control(&handle_logout, provider_id, "disconnected");
    });

    // ── provider_data_updated ────────────────────────────────────────────────
    let overlay_clone = overlay.clone();
    overlay.listen("provider_data_updated", move |event| {
        println!("[DEBUG] 收到 provider_data_updated 事件");
        let Ok(json) = serde_json::from_str::<serde_json::Value>(event.payload()) else {
            eprintln!("[ERROR] 无效 payload: {}", event.payload());
            return;
        };
        let pid = json.get("provider_id").and_then(|v| v.as_str()).unwrap_or("?");
        println!("[Monitor] 数据更新: provider_id={}", pid);
        overlay::push_provider_data(&overlay_clone, &json);
    });

    // ── overlay_active_tab_changed ───────────────────────────────────────────────────────────────────────
    // 当悬浮窗切换 TAB 或激活 TAB 的数据更新时，由 overlay JS 发出此事件。
    // Windows：更新任务栏组件（第一行改为 TAB 名称）
    // macOS：更新托盘标题（格式 "N)₁₂₃₄"，N 为 TAB 序号）
    overlay.listen("overlay_active_tab_changed", move |event| {
        println!("[DEBUG] 收到 overlay_active_tab_changed 事件");
        let Ok(json) = serde_json::from_str::<serde_json::Value>(event.payload()) else {
            eprintln!("[ERROR] overlay_active_tab_changed payload 解析失败");
            return;
        };
        let pid = json.get("provider_id").and_then(|v| v.as_str()).unwrap_or("?");
        let tab = json.get("tab_index").and_then(|v| v.as_u64()).unwrap_or(0);
        println!("[Monitor] 激活 TAB 变更: provider_id={}, tab_index={}", pid, tab);

        #[cfg(target_os = "windows")]
        windows_taskbar::update_widget_data(&json);

        #[cfg(target_os = "macos")]
        macos_tray::update_active_tab_tray(&json);
    });
}

fn notify_control(handle: &AppHandle, provider_id: &str, status: &str) {
    println!("[DEBUG] notify_control: provider_id={}, status={}", provider_id, status);

    // 同步更新 Rust 侧状态追踪
    {
        let state = handle.state::<AppState>();
        let mut map = state.provider_status.lock().unwrap();
        map.insert(provider_id.to_string(), status.to_string());
    }

    if let Some(ctrl) = handle.get_webview_window("control") {
        let js = format!(
            "window.__onProviderStatus && window.__onProviderStatus('{}', '{}');",
            provider_id, status
        );
        if let Err(e) = ctrl.eval(&js) {
            eprintln!("[ERROR] 通知控制面板失败: {}", e);
        } else {
            println!("[DEBUG] 控制面板通知成功");
        }
    } else {
        eprintln!("[WARN] 未找到控制面板窗口");
    }
}
