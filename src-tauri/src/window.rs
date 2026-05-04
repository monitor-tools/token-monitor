//! 控制面板窗口模块
//!
//! 控制面板是用户连接各 Provider 的入口：
//! - 启动时自动显示，固定尺寸（460×320），内容不超出窗口
//! - 关闭按钮 → 隐藏到托盘（不退出应用）
//! - 所有 Provider 连接成功后由 JS 调用 `hide_control_panel` 自动收起
//!
//! # 关闭按钮说明
//! `on_window_event` 在 Windows 上由消息循环（主线程）触发；
//! 但为避免 WebView2 在某些版本中要求 `hide()` 必须经过主线程分发，
//! 统一通过 `run_on_main_thread` 保证线程安全。

use tauri::{AppHandle, WebviewUrl, WebviewWindowBuilder, WindowEvent};

pub fn create_control_panel(handle: &AppHandle) -> tauri::Result<()> {
    println!("[DEBUG] 开始创建控制面板窗口...");
    
    let control = WebviewWindowBuilder::new(
        handle,
        "control",
        WebviewUrl::App("control.html".into()),
    )
    .title("Code Plan 套餐余量监控")
    .inner_size(580.0, 345.0)
    .min_inner_size(580.0, 220.0)
    .resizable(true)
    .center()
    .build()?;

    println!("[DEBUG] ✓ 控制面板窗口创建成功");
    
    // 确保窗口可见
    if let Err(e) = control.show() {
        eprintln!("[ERROR] 显示控制面板失败: {}", e);
    } else {
        println!("[DEBUG] 控制面板已显示");
    }
    
    if let Err(e) = control.set_focus() {
        eprintln!("[ERROR] 设置控制面板焦点失败: {}", e);
    } else {
        println!("[DEBUG] 控制面板焦点已设置");
    }

    // 关闭按钮 → 阻止系统关闭，改为隐藏到托盘
    // 通过 run_on_main_thread 分发确保 Windows/WebView2 线程安全
    let ctrl   = control.clone();
    let handle = handle.clone();
    control.on_window_event(move |event| {
        if let WindowEvent::CloseRequested { api, .. } = event {
            api.prevent_close();

            let w = ctrl.clone();
            if let Err(e) = handle.run_on_main_thread(move || {
                if let Err(e) = w.hide() {
                    eprintln!("[window] 隐藏控制面板失败: {e}");
                }
            }) {
                eprintln!("[window] run_on_main_thread 失败: {e}");
            }
        }
    });

    Ok(())
}
