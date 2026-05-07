//! 悬浮信息窗口模块
//!
//! 行为说明：
//! - 拖动中：自动限制在显示器范围内；进入边缘阈值后吸附
//! - 鼠标离开（贴边时）：根据窗口中心所在象限确定角落，同时收缩尺寸并移到该角落
//! - 鼠标进入（折叠时）：展开并将窗口锚定在相同角落，鼠标不会立即触发再次离开
//! - 防抖：Overlay HTML 侧记录上次模式切换时间，冷却期内忽略 enter/leave 事件
//!
//! # 跨平台说明
//! - **Windows**: `skip_taskbar(true)` 有效；拖拽吸附依赖 `Moved` 事件正常触发
//! - **macOS**: `skip_taskbar` 无效，需 `LSUIElement=YES` 或 activation policy；
//!   `set_size` 存在异步延迟，Corner 函数通过「先读旧尺寸算坐标，再写新尺寸+位置」规避
//! - **透明窗口**: Windows 需 WebView2；macOS 通过 Tauri 自动支持

use tauri::{
    AppHandle, Listener, PhysicalPosition, Position, Size, WebviewUrl,
    WebviewWindowBuilder, WindowEvent,
};
use std::sync::atomic::{AtomicBool, Ordering};

// ─── 常量 ─────────────────────────────────────────────────────────────────────

pub const EXPANDED_W: u32 = 340;
pub const EXPANDED_H: u32 = 250;

// 标记 overlay 窗口是否已经初始化显示过
static OVERLAY_INITIALIZED: AtomicBool = AtomicBool::new(false);

// ─── 窗口创建 ─────────────────────────────────────────────────────────────────

/// 创建透明悬浮窗（初始隐藏）。
/// 第一个 Provider 上报数据时，通过 [`push_provider_data`] 调用 [`show_initial`] 显示。
pub fn create_overlay(handle: &AppHandle) -> tauri::Result<tauri::WebviewWindow> {
    #[cfg(target_os = "macos")]
    let overlay = WebviewWindowBuilder::new(
        handle,
        "overlay",
        WebviewUrl::App("overlay.html".into()),
    )
    .title("Code Plan 套餐余量监控")
    .inner_size(EXPANDED_W as f64, EXPANDED_H as f64)
    .maximizable(false)
    .minimizable(false)
    .resizable(false)
    .decorations(false)
    .transparent(true)
    .always_on_top(true)
    .skip_taskbar(true)
    .visible(false)
    .build()?;

    #[cfg(not(target_os = "macos"))]
    let overlay = WebviewWindowBuilder::new(
        handle,
        "overlay",
        WebviewUrl::App("overlay.html".into()),
    )
    .title("Code Plan 套餐余量监控")
    .inner_size(EXPANDED_W as f64, EXPANDED_H as f64)
    .maximizable(false)
    .minimizable(false)
    .resizable(false)
    .decorations(false)
    .transparent(true)
    .shadow(false) // Windows: 去除系统默认的 1px 边框
    .always_on_top(true)
    .skip_taskbar(false) // Windows: 改为 false，避免窗口被系统隐藏
    .visible(false)
    .build()?;

    // 拖动：仅限制在屏幕内，不自动吸附
    // 自动靠边会在拖动过程中反复触发，导致窗口抖动；
    // 靠边/吸附只在用户手动切换折叠模式时执行。
    let ov = overlay.clone();
    overlay.on_window_event(move |event| {
        if let WindowEvent::Moved(_) = event {
            clamp_to_monitor(&ov);
        }
    });

    Ok(overlay)
}

// ─── 悬浮窗事件 ───────────────────────────────────────────────────────────────

/// 注册 overlay 的手动模式切换事件。
///
/// 展开/折叠由右上角按钮触发，不再依赖鼠标进入/离开自动切换，
/// 避免拖动时因边缘检测触发意外的折叠/展开。
/// 折叠时自动吸附到最近屏幕角落；展开时从同一角落展开。
pub fn setup_overlay_events(overlay: &tauri::WebviewWindow) {
    // 窗口尺寸调整事件（前端根据内容高度请求调整）
    let ov_resize = overlay.clone();
    overlay.listen("overlay_adjust_size", move |event| {
        let payload = event.payload();
        if let Ok(size_data) = serde_json::from_str::<serde_json::Value>(payload) {
            if let (Some(width), Some(height)) = (
                size_data["width"].as_u64(),
                size_data["height"].as_u64(),
            ) {
                // 所有平台统一使用逻辑尺寸（前端发送的是逻辑像素）
                let new_size = tauri::LogicalSize::new(width as f64, height as f64);
                let _ = ov_resize.set_size(Size::Logical(new_size));
                eprintln!("[overlay] Window size adjusted to: {}x{}", width, height);
            }
        }
    });

    // 置顶切换事件
    let ov_pin = overlay.clone();
    overlay.listen("overlay_toggle_pin", move |_| {
        eprintln!("[overlay] toggle_pin event received");
        
        // 获取当前置顶状态
        let Ok(is_pinned) = ov_pin.is_always_on_top() else {
            eprintln!("[overlay] Failed to get always_on_top state");
            return;
        };
        
        // 切换置顶状态
        let new_state = !is_pinned;
        if let Err(e) = ov_pin.set_always_on_top(new_state) {
            eprintln!("[overlay] Failed to set always_on_top: {}", e);
            return;
        }
        
        eprintln!("[overlay] Always on top set to: {}", new_state);
        
        // 更新前端按钮状态
        let script = format!("window.updatePinState({});", new_state);
        let _ = ov_pin.eval(&script);
    });

    // 响应前端请求初始置顶状态
    let ov_state = overlay.clone();
    overlay.listen("overlay_request_pin_state", move |_| {
        eprintln!("[overlay] request_pin_state event received");
        
        if let Ok(is_pinned) = ov_state.is_always_on_top() {
            let script = format!("window.updatePinState({});", is_pinned);
            let _ = ov_state.eval(&script);
            eprintln!("[overlay] Initial pin state sent: {}", is_pinned);
        }
    });
}

// ─── 数据推送 ─────────────────────────────────────────────────────────────────

/// 将 Provider 数据推入 Overlay 的 JS 状态。
///
/// 若窗口尚未初始化显示过（首次数据到达），调用 [`show_initial`] 将其定位到屏幕右下角并显示。
/// 若窗口已初始化过（无论当前是否可见），仅刷新数据，不改变当前显示状态。
pub fn push_provider_data(overlay: &tauri::WebviewWindow, payload: &serde_json::Value) {
    let script = format!("window.updateProviderData({});", payload);
    let _ = overlay.eval(&script);

    // 检查是否是首次数据到达
    let already_initialized = OVERLAY_INITIALIZED.load(Ordering::Relaxed);
    
    if !already_initialized {
        eprintln!("[overlay] First data received, showing window");
        show_initial(overlay);
        OVERLAY_INITIALIZED.store(true, Ordering::Relaxed);
    } else {
        eprintln!("[overlay] Data updated, keeping current visibility state");
    }
}

/// 首次显示 Overlay：定位到屏幕右下角（距边缘留 20px 余量以露出任务栏）。
pub fn show_initial(overlay: &tauri::WebviewWindow) {
    // 所有平台统一使用逻辑尺寸
    let _ = overlay.set_size(Size::Logical(tauri::LogicalSize::new(EXPANDED_W as f64, EXPANDED_H as f64)));
    
    // Windows: 确保窗口可见且置顶
    #[cfg(target_os = "windows")]
    {
        let _ = overlay.set_always_on_top(true);
        let _ = overlay.set_skip_taskbar(false);
    }
    
    let _ = overlay.show();
    let _ = overlay.set_focus();

    // show() 后 current_monitor 有值
    if let Ok(Some(monitor)) = overlay.current_monitor() {
        let mp = monitor.position();
        let ms = monitor.size();
        let x = (mp.x + ms.width as i32 - EXPANDED_W as i32 - 20).max(mp.x);
        let y = (mp.y + ms.height as i32 - EXPANDED_H as i32 - 60).max(mp.y);
        let _ = overlay.set_position(Position::Physical(PhysicalPosition::new(x, y)));
    }
}

// ─── 位置工具函数 ─────────────────────────────────────────────────────────────

/// 将窗口限制在当前显示器可见区域内（防止拖出屏幕外）。
pub fn clamp_to_monitor(overlay: &tauri::WebviewWindow) {
    let Ok(Some(monitor)) = overlay.current_monitor() else { return };
    let Ok(pos)  = overlay.outer_position() else { return };
    let Ok(size) = overlay.outer_size()     else { return };

    let mp = monitor.position();
    let ms = monitor.size();

    let max_x = (mp.x + ms.width as i32  - size.width as i32).max(mp.x);
    let max_y = (mp.y + ms.height as i32 - size.height as i32).max(mp.y);

    let cx = pos.x.clamp(mp.x, max_x);
    let cy = pos.y.clamp(mp.y, max_y);

    if cx != pos.x || cy != pos.y {
        let _ = overlay.set_position(Position::Physical(PhysicalPosition::new(cx, cy)));
    }
}


