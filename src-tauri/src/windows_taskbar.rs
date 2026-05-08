//! Windows 任务栏文字组件模块
//!
//! 使用**浮动置顶窗口**在系统托盘（通知区域）左侧显示 5 小时剩余配额。
//!
//! ## 设计要点
//! - **不嵌入任务栏**：不调用 `SetParent`，避免 Windows 11 兼容性问题
//!   （嵌入 Shell_TrayWnd 后 SetWindowPos 可能被系统忽略，导致停留在位置 (0,0)）
//! - **屏幕坐标定位**：以 HWND_TOPMOST 浮于任务栏上方，用真实屏幕坐标定位
//! - **动态避让**：启动时和每 5 秒扫描任务栏子窗口及其他浮动窗口，
//!   自动找到不与其他软件叠加的位置

use tauri::{AppHandle, Manager};
use std::sync::{Mutex, Arc};
use std::sync::atomic::{AtomicPtr, Ordering};

use std::sync::atomic::AtomicU32;


use windows::Win32::{
    Foundation::{BOOL, HWND, LPARAM, LRESULT, RECT, COLORREF, WPARAM},
    UI::WindowsAndMessaging::*,
    Graphics::Gdi::*,
    System::LibraryLoader::GetModuleHandleW,
};

// ─── 常量 ────────────────────────────────────────────────────────────────────

const WIDGET_WIDTH:  i32 = 80;
const WIDGET_HEIGHT: i32 = 40;

/// 定时重新定位的 Timer ID
const TIMER_REPOSITION: usize = 1001;

// ─── 全局状态 ─────────────────────────────────────────────────────────────────

/// 存储配额文本（两行：标签 + 数字）

static QUOTA_TEXT: Mutex<Option<(String, String)>> = Mutex::new(None);

/// 存储窗口句柄（用于跨线程触发重绘）

static WIDGET_HWND: AtomicPtr<std::ffi::c_void> = AtomicPtr::new(std::ptr::null_mut());

/// 存储 AppHandle（用于点击事件切换悬浮窗）

static APP_HANDLE: Mutex<Option<Arc<AppHandle>>> = Mutex::new(None);

/// 动态颜色（COLORREF 格式：0x00BBGGRR）

static WIDGET_BG_COLOR:   AtomicU32 = AtomicU32::new(0x00242611); // 深灰背景 #242611

static WIDGET_TEXT_COLOR: AtomicU32 = AtomicU32::new(0x00999999); // 文字颜色 #999999

/// 上次成功定位的坐标（屏幕坐标），用于溢出面板过渡期的位置保护
/// 初始值 i32::MIN 表示「尚未记录」

use std::sync::atomic::AtomicI32;
static LAST_GOOD_X: AtomicI32 = AtomicI32::new(i32::MIN);
static LAST_GOOD_Y: AtomicI32 = AtomicI32::new(i32::MIN);

/// 上次实际调用 SetWindowPos 时使用的坐标，用于「位置未变则跳过」优化
static LAST_SET_X: AtomicI32 = AtomicI32::new(i32::MIN);
static LAST_SET_Y: AtomicI32 = AtomicI32::new(i32::MIN);

/// Previous tray_left value. Used to detect when TrayNotifyWnd layout is in transition
/// (e.g. overflow panel opening/closing). When tray_left changes suddenly, we skip
/// recomputing position and hold the last known-good location until it stabilizes.
static PREV_TRAY_LEFT: AtomicI32 = AtomicI32::new(i32::MIN);

// ─── 公共接口 ────────────────────────────────────────────────────────────────

/// 创建任务栏文字组件（仅 Windows）

pub fn create_taskbar_widget(handle: &AppHandle) -> tauri::Result<()> {
    eprintln!("[taskbar] 创建浮动任务栏组件（不嵌入 Shell_TrayWnd）");

    if let Ok(mut g) = APP_HANDLE.lock() {
        *g = Some(Arc::new(handle.clone()));
    }

    std::thread::Builder::new()
        .name("taskbar_widget".into())
        .spawn(move || unsafe {
            if let Err(e) = create_native_window() {
                eprintln!("[taskbar] 窗口创建失败: {:?}", e);
            }
        })
        .map(|_| ())
        .map_err(tauri::Error::Io)
}

/// 设置配额数据更新事件监听
///
/// 注意：实际更新逻辑已移入 main.rs 的 setup_global_events（overlay.listen 路径），
/// 由此确保与悬浮窗使用同一事件接收路径（AppHandle::listen 在 Tauri v2 中
/// 无法接收 JavaScript WebView 发出的事件）。
/// 此函数保留以维持公共 API 形态，内部为空实现。

#[allow(unused_variables)]
pub fn setup_taskbar_widget_events(handle: &AppHandle) {
    // 数据更新现由 setup_global_events 中的 overlay.listen 路径驱动
}

/// 显示任务栏组件（创建时已自动显示，此函数为 API 兼容保留）

#[allow(dead_code)]
pub fn show_taskbar_widget() {}

/// 更新小组件颜色（由控制面板保存触发）。
/// `bg_hex`、`label_hex`、`value_hex` 均为 `#RRGGBB` 格式。

pub fn set_widget_colors(bg_hex: &str, text_hex: &str) {
    WIDGET_BG_COLOR.store(html_to_colorref(bg_hex), Ordering::Relaxed);
    WIDGET_TEXT_COLOR.store(html_to_colorref(text_hex), Ordering::Relaxed);

    // 触发重绘（bErase=false：paint_widget 已在双缓冲中自行填充背景）
    let ptr = WIDGET_HWND.load(Ordering::Relaxed);
    if !ptr.is_null() {
        unsafe {
            let hwnd = HWND(ptr);
            let _ = InvalidateRect(hwnd, None, false);
        }
    }
    eprintln!("[taskbar] 颜色已更新: bg={} text={}", bg_hex, text_hex);
}

/// 将 `#RRGGBB` 颜色字符串转换为 Windows COLORREF（0x00BBGGRR）

fn html_to_colorref(hex: &str) -> u32 {
    let hex = hex.trim_start_matches('#');
    if hex.len() < 6 { return 0; }
    let r = u32::from_str_radix(&hex[0..2], 16).unwrap_or(0);
    let g = u32::from_str_radix(&hex[2..4], 16).unwrap_or(0);
    let b = u32::from_str_radix(&hex[4..6], 16).unwrap_or(0);
    (b << 16) | (g << 8) | r
}

// ─── 窗口创建 ────────────────────────────────────────────────────────────────


unsafe fn create_native_window() -> windows::core::Result<()> {
    use windows::core::w;

    let class_name = w!("TaskbarQuotaWidget_v2");
    let hinstance  = GetModuleHandleW(None)?;

    // 注册窗口类（忽略重复注册错误，深灰色背景与任务栏融合）
    let wc = WNDCLASSW {
        style:         CS_NOCLOSE,
        lpfnWndProc:   Some(window_proc),
        hInstance:     hinstance.into(),
        hCursor:       LoadCursorW(None, IDC_ARROW)?,
        hbrBackground: CreateSolidBrush(COLORREF(0x00242611)),
        lpszClassName: class_name,
        ..Default::default()
    };
    let _ = RegisterClassW(&wc); // 忽略 ERROR_CLASS_ALREADY_EXISTS

    // 创建浮动置顶窗口
    // - WS_EX_TOOLWINDOW  : 不在任务栏显示按钮
    // - WS_EX_TOPMOST     : 始终在普通窗口上方（与任务栏同层）
    // - WS_EX_NOACTIVATE  : 点击不抢焦点
    // 初始位置放在屏幕外，position_near_tray() 调用后再移到正确位置
    let hwnd = CreateWindowExW(
        WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
        class_name,
        w!("配额监控"),
        WS_POPUP,
        -2000, -2000,
        WIDGET_WIDTH, WIDGET_HEIGHT,
        None, None, hinstance, None,
    )?;

    eprintln!("[taskbar] 浮动窗口已创建: {:?}", hwnd);
    WIDGET_HWND.store(hwnd.0 as *mut _, Ordering::Relaxed);

    // 定位到系统托盘左侧（此时窗口还不可见，不会被 collect_floating_widgets 自身检测到）
    position_near_tray(hwnd);

    // 显示窗口（不抢焦点）
    let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    let _ = UpdateWindow(hwnd);

    // 每 1 秒重新检查一次位置（加速从溢出托盘面板过渡期的位置异常中恢复）
    let _ = SetTimer(hwnd, TIMER_REPOSITION, 1000, None);

    // 消息循环
    let mut msg = MSG::default();
    while GetMessageW(&mut msg, None, 0, 0).into() {
        let _ = TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }

    Ok(())
}

// ─── 消息处理 ────────────────────────────────────────────────────────────────


unsafe extern "system" fn window_proc(
    hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_PAINT => paint_widget(hwnd),

        // 阻止系统用类刷子擦除背景（避免颜色切换时闪烁）
        // WM_PAINT 负责完整绘制包括背景
        WM_ERASEBKGND => LRESULT(1),

        // 数据更新通知（由 update_widget_data 跨线程 PostMessageW 发来）
        // 在消息循环线程中安全执行重绘，避免跨线程 GDI 问题。
        WM_APP => {
            let _ = InvalidateRect(hwnd, None, false);
            let _ = UpdateWindow(hwnd);
            LRESULT(0)
        }

        WM_TIMER if wparam.0 == TIMER_REPOSITION => {
            // 定期检查并调整位置，避让其他新出现的软件
            position_near_tray(hwnd);
            LRESULT(0)
        }

        WM_LBUTTONDOWN => {
            // 点击切换悬浮信息窗显示 / 隐藏
            std::thread::spawn(|| {
                if let Ok(g) = APP_HANDLE.lock() {
                    if let Some(arc) = g.as_ref() {
                        if let Some(ov) = arc.get_webview_window("overlay") {
                            let visible = ov.is_visible().unwrap_or(false);
                            if visible {
                                let _ = ov.hide();
                            } else {
                                let _ = ov.show();
                                let _ = ov.set_focus();
                            }
                        }
                    }
                }
            });
            LRESULT(0)
        }

        WM_DESTROY => {
            let _ = KillTimer(hwnd, TIMER_REPOSITION);
            PostQuitMessage(0);
            LRESULT(0)
        }

        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// 绘制窗口内容（两行文字：绿色标签 + 白色数字）
///
/// 使用**双缓冲**：先在内存 DC 中完成所有绘制，再一次性 BitBlt 到屏幕，
/// 避免 FillRect 与 DrawTextW 之间的闪烁。

unsafe fn paint_widget(hwnd: HWND) -> LRESULT {
    let mut ps = PAINTSTRUCT::default();
    let hdc = BeginPaint(hwnd, &mut ps);

    let mut rect = RECT::default();
    let _ = GetClientRect(hwnd, &mut rect);
    let width  = rect.right - rect.left;
    let height = rect.bottom - rect.top;

    // ── 双缓冲：创建离屏 DC 和位图 ──────────────────────────────────────────
    let mem_dc   = CreateCompatibleDC(hdc);
    let mem_bmp  = CreateCompatibleBitmap(hdc, width, height);
    let old_bmp  = SelectObject(mem_dc, mem_bmp);

    // 读取动态颜色
    let bg_color   = WIDGET_BG_COLOR.load(Ordering::Relaxed);
    let text_color = WIDGET_TEXT_COLOR.load(Ordering::Relaxed);

    // 用当前背景色填充整个离屏客户区
    let bg_brush = CreateSolidBrush(COLORREF(bg_color));
    FillRect(mem_dc, &rect, bg_brush);
    let _ = DeleteObject(bg_brush);

    // 文字背景不遮盖底色
    SetBkMode(mem_dc, TRANSPARENT);

    // 读取最新配额数据
    let (line1, line2) = QUOTA_TEXT.lock()
        .ok()
        .and_then(|g| g.as_ref().map(|(a, b)| (a.clone(), b.clone())))
        .unwrap_or_else(|| ("".into(), "加载中...".into()));

    let font = CreateFontW(
        14, 0, 0, 0,
        FW_BOLD.0 as i32,
        0, 0, 0,
        DEFAULT_CHARSET.0 as u32,
        OUT_DEFAULT_PRECIS.0 as u32,
        CLIP_DEFAULT_PRECIS.0 as u32,
        DEFAULT_QUALITY.0 as u32,
        (DEFAULT_PITCH.0 | FF_DONTCARE.0) as u32,
        windows::core::w!("Microsoft YaHei"),
    );
    let old_font = SelectObject(mem_dc, font);

    SetTextColor(mem_dc, COLORREF(text_color));

    if line1.is_empty() {
        // 无数据或未登录：仅显示一行，垂直居中
        let mut w2: Vec<u16> = line2.encode_utf16().chain(Some(0)).collect();
        let mut r2 = rect;
        r2.top = (height - 14) / 2;
        r2.bottom = r2.top + 14;
        DrawTextW(mem_dc, &mut w2, &mut r2, DT_CENTER | DT_SINGLELINE);
    } else {
        // 有数据：两行显示
        let mut w1: Vec<u16> = line1.encode_utf16().chain(Some(0)).collect();
        let mut r1 = rect;
        r1.top = 4; r1.bottom = 20;
        DrawTextW(mem_dc, &mut w1, &mut r1, DT_CENTER | DT_SINGLELINE);

        let mut w2: Vec<u16> = line2.encode_utf16().chain(Some(0)).collect();
        let mut r2 = rect;
        r2.top = 22; r2.bottom = 38;
        DrawTextW(mem_dc, &mut w2, &mut r2, DT_CENTER | DT_SINGLELINE);
    }

    SelectObject(mem_dc, old_font);
    let _ = DeleteObject(font);

    // ── 一次性将离屏内容拷贝到屏幕 ──────────────────────────────────────────
    let _ = BitBlt(
        hdc,
        rect.left, rect.top, width, height,
        mem_dc,
        rect.left, rect.top,
        SRCCOPY,
    );

    // 清理双缓冲资源
    SelectObject(mem_dc, old_bmp);
    let _ = DeleteObject(mem_bmp);
    let _ = DeleteDC(mem_dc);

    let _ = EndPaint(hwnd, &ps);

    LRESULT(0)
}

// ─── 定位核心逻辑 ─────────────────────────────────────────────────────────────

/// 将浮动窗口定位到系统托盘左侧，动态避让其他软件占用的区域。
///
/// 步骤：
/// 1. 找到 `Shell_TrayWnd` → 取得任务栏屏幕位置
/// 2. 找到 `TrayNotifyWnd` → 取得通知区域左边界
/// 3. 收集两类占用区间：任务栏子窗口 + 其他浮动置顶窗口
/// 4. 用 `find_free_x` 从托盘左侧向左寻找空闲位置
/// 5. 调用 `SetWindowPos` 移动窗口

unsafe fn position_near_tray(hwnd: HWND) {
    use windows::core::PCWSTR;

    // ── 1. 找到任务栏 ────────────────────────────────────────────────────────
    let tb_class = to_wide("Shell_TrayWnd");
    let taskbar = match FindWindowW(PCWSTR(tb_class.as_ptr()), PCWSTR::null()) {
        Ok(h) if !h.is_invalid() => h,
        _ => {
            eprintln!("[taskbar] 未找到 Shell_TrayWnd，跳过定位");
            return;
        }
    };

    let mut tb_rect = RECT::default();
    if GetWindowRect(taskbar, &mut tb_rect).is_err() {
        return;
    }
    let tb_h = tb_rect.bottom - tb_rect.top;

    // ── 1.5 任务栏为前台时立即重新提升 Z-order ─────────────────────────────
    //
    // 用户点击任务栏背景或任务按钮时，Shell_TrayWnd 会变成前台窗口，
    // Windows 将其提到 TOPMOST 层顶部，把我们的 widget 压到任务栏后面。
    //
    // 修复策略（仿自 TrafficMonitor AdjustWindowPos）：
    //   检测到前台窗口 == Shell_TrayWnd 时，立即调用 SetWindowPos(HWND_TOPMOST)。
    //
    // 为什么不会引起 overlay 闪烁：
    //   overlay 弹出时前台窗口是 overlay 的 HWND，而非 Shell_TrayWnd，
    //   因此此判断正好天然与两种情况互斥。
    let fg = GetForegroundWindow();
    if fg == taskbar {
        let lx = LAST_SET_X.load(Ordering::Relaxed);
        let ly = LAST_SET_Y.load(Ordering::Relaxed);
        if lx != i32::MIN {
            eprintln!("[taskbar] 任务栏为前台，重新提升 Z-order");
            let _ = SetWindowPos(
                hwnd, HWND_TOPMOST,
                lx, ly, WIDGET_WIDTH, WIDGET_HEIGHT,
                SWP_NOACTIVATE,
            );
        }
    }

    // ── 2. 找到系统通知区域，取其左边界（屏幕坐标）────────────────────────────
    let tray_class = to_wide("TrayNotifyWnd");
    let tray_left = match FindWindowExW(
        taskbar, None,
        PCWSTR(tray_class.as_ptr()), PCWSTR::null(),
    ) {
        Ok(h) if !h.is_invalid() => {
            let mut r = RECT::default();
            if GetWindowRect(h, &mut r).is_ok() {
                eprintln!("[taskbar] TrayNotifyWnd 左边界(屏幕坐标): {}", r.left);
                r.left
            } else {
                tb_rect.right - 180
            }
        }
        _ => {
            eprintln!("[taskbar] 未找到 TrayNotifyWnd，估算位置");
            tb_rect.right - 180
        }
    };

    eprintln!(
        "[taskbar] 任务栏: left={} right={} top={} bottom={}, 托盘左边界: {}",
        tb_rect.left, tb_rect.right, tb_rect.top, tb_rect.bottom, tray_left
    );

    // ── 2.5 tray_left 稳定性检查 ─────────────────────────────────────────────
    //
    // 当「显示更多托盘图标」溢出面板弹出/收起时，TrayNotifyWnd 会短暂报告
    // 一个异常的 left 值（不同于平时）。若我们直接用这个瞬变值重新计算位置，
    // 会算出一个处于任务按钮区中间的坐标，widget 移过去后视觉上「消失」。
    //
    // 解决策略：
    //   - 把上一帧的 tray_left 记录在 PREV_TRAY_LEFT
    //   - 若本帧 tray_left 与上帧不同 → 布局正在过渡，本帧跳过重新计算，
    //     仅重置 Z-order（防止被其他 TOPMOST 窗口遮住）并 return
    //   - 若本帧与上帧相同（或为首次） → 布局稳定，正常计算并应用位置
    let prev_tl = PREV_TRAY_LEFT.swap(tray_left, Ordering::Relaxed);
    let tray_just_changed = prev_tl != i32::MIN && prev_tl != tray_left;
    if tray_just_changed {
        eprintln!(
            "[taskbar] tray_left 从 {} 变为 {}，布局过渡中，保持当前位置",
            prev_tl, tray_left
        );
        // widget 已在正确位置，直接跳过即可；不调用 SetWindowPos，
        // 避免不必要的 Z-order 改变引起 overlay 等窗口闪烁
        return;
    }

    // ── 3. 收集已被占用的 X 区间（屏幕坐标） ─────────────────────────────────
    let mut occupied: Vec<(i32, i32)> = Vec::new();

    // 3a. Shell_TrayWnd 内由第三方软件注入的子窗口
    occupied.extend(collect_taskbar_children(taskbar, &tb_rect, tray_left, hwnd));

    // 3b. 屏幕上其他位于任务栏区域的浮动置顶窗口
    occupied.extend(collect_floating_widgets(&tb_rect, tray_left, hwnd));

    eprintln!("[taskbar] 共 {} 个占用区间: {:?}", occupied.len(), occupied);

    // ── 4. 寻找第一个空闲位置 ───────────────────────────────────────────────
    let start_x = tray_left - WIDGET_WIDTH - 4; // 从托盘左侧 4px 处开始

    // 不超过任务栏左侧 1/3（避免压到任务按钮区）
    let taskbar_span = tb_rect.right - tb_rect.left;
    let min_x = tb_rect.left + taskbar_span / 3;

    let x = find_free_x(start_x, WIDGET_WIDTH, &occupied, min_x);

    // ── 5. Y 坐标：在任务栏高度内垂直居中 ─────────────────────────────────────
    let y = tb_rect.top + (tb_h - WIDGET_HEIGHT).max(0) / 2;

    eprintln!("[taskbar] 最终屏幕坐标: ({}, {}), 尺寸: {}×{}", x, y, WIDGET_WIDTH, WIDGET_HEIGHT);

    // ── 6. 位置合法性校验 ────────────────────────────────────────────────────
    //
    // 当「显示更多托盘图标」弹出面板打开或关闭时，Windows 会短暂重新布局任务栏，
    // 此期间 TrayNotifyWnd 可能返回异常 Rect，导致计算结果跑到屏幕外。
    // 判据：
    //   - tray_left 必须在 (tb_rect.left + 50, tb_rect.right) 之间（合理的通知区域左边界）
    //   - x 必须在 [tb_rect.left, tray_left - WIDGET_WIDTH] 之间
    //   - y 必须在任务栏垂直范围内（允许 ±5px 容差）
    let tray_ok = tray_left > tb_rect.left + 50 && tray_left < tb_rect.right;
    let x_ok    = x >= tb_rect.left && x + WIDGET_WIDTH <= tray_left + 10;
    let y_ok    = y >= tb_rect.top - 5 && y + WIDGET_HEIGHT <= tb_rect.bottom + 5;

    // ── 7. 应用位置 ────────────────────────────────────────────────────────────────
    // tray_left 已稳定（未发生过渡），可以直接计算并应用位置。

    let (final_x, final_y) = if tray_ok && x_ok && y_ok {
        // 合法位置：更新历史好位置记录
        LAST_GOOD_X.store(x, Ordering::Relaxed);
        LAST_GOOD_Y.store(y, Ordering::Relaxed);
        (x, y)
    } else {
        // 位置异常：复用上次好位置
        let lx = LAST_GOOD_X.load(Ordering::Relaxed);
        let ly = LAST_GOOD_Y.load(Ordering::Relaxed);
        if lx != i32::MIN {
            eprintln!(
                "[taskbar] 位置异常(tray_ok={} x_ok={} y_ok={})，复用上次好位置 ({}, {})",
                tray_ok, x_ok, y_ok, lx, ly
            );
            (lx, ly)
        } else {
            eprintln!("[taskbar] 位置异常且无历史记录，跳过本次定位");
            return;
        }
    };

    // 位置未变 → 什么也不做，直接跳过
    // （连 Z-order 重置也不调：每秒贺 Z-order 会把 widget 抬到 overlay、
    //  候选框等窗口之上，导致点击时闪烁）
    let last_set_x = LAST_SET_X.load(Ordering::Relaxed);
    let last_set_y = LAST_SET_Y.load(Ordering::Relaxed);
    if final_x == last_set_x && final_y == last_set_y {
        return;
    }

    // 位置发生变化 → 立即移动
    eprintln!("[taskbar] 执行 SetWindowPos ({}, {})", final_x, final_y);
    LAST_SET_X.store(final_x, Ordering::Relaxed);
    LAST_SET_Y.store(final_y, Ordering::Relaxed);

    let _ = SetWindowPos(
        hwnd, HWND_TOPMOST,
        final_x, final_y, WIDGET_WIDTH, WIDGET_HEIGHT,
        SWP_NOACTIVATE,
    );
}

// ─── 占用区间收集 ─────────────────────────────────────────────────────────────

/// 枚举 `Shell_TrayWnd` 内由第三方软件注入的子窗口，返回其屏幕 X 占用区间。
///
/// 已知的 Windows 系统窗口类（`ReBarWindow32`、`TrayNotifyWnd` 等）会被自动跳过。

unsafe fn collect_taskbar_children(
    taskbar: HWND,
    tb_rect: &RECT,
    tray_left: i32,
    own_hwnd: HWND,
) -> Vec<(i32, i32)> {
    struct State {
        regions: Vec<(i32, i32)>,
        tb_top: i32, tb_bottom: i32, tb_left: i32,
        tray_left: i32, own: HWND,
    }

    unsafe extern "system" fn cb(child: HWND, lp: LPARAM) -> BOOL {
        let s = &mut *(lp.0 as *mut State);

        if child == s.own || !IsWindowVisible(child).as_bool() {
            return true.into();
        }

        let mut buf = [0u16; 256];
        let n = GetClassNameW(child, &mut buf);
        if n == 0 { return true.into(); }
        let cls = String::from_utf16_lossy(&buf[..n as usize]);
        if is_system_window_class(&cls) { return true.into(); }

        let mut r = RECT::default();
        if GetWindowRect(child, &mut r).is_err() { return true.into(); }

        // 过滤太小的窗口（可能是内部控件）
        if r.right - r.left < 8 || r.bottom - r.top < 8 { return true.into(); }

        // 必须在任务栏垂直范围内
        if r.bottom <= s.tb_top || r.top >= s.tb_bottom { return true.into(); }

        // 必须在感兴趣的水平区域（任务栏左端到托盘左端之间）
        if r.right <= s.tb_left || r.left >= s.tray_left { return true.into(); }

        let left  = r.left.max(s.tb_left);
        let right = r.right.min(s.tray_left);
        if right > left {
            eprintln!("[taskbar] 子窗口占用 class={} [{}, {}]", cls, left, right);
            s.regions.push((left, right));
        }
        true.into()
    }

    let mut state = State {
        regions:   Vec::new(),
        tb_top:    tb_rect.top,
        tb_bottom: tb_rect.bottom,
        tb_left:   tb_rect.left,
        tray_left,
        own:       own_hwnd,
    };

    let _ = EnumChildWindows(
        taskbar, Some(cb),
        LPARAM(&mut state as *mut State as isize),
    );
    state.regions
}

/// 枚举屏幕上位于任务栏区域（托盘左侧）的其他浮动置顶窗口，返回屏幕 X 占用区间。
///
/// 这可以检测到未嵌入任务栏但悬浮在任务栏位置上方的第三方小工具。

unsafe fn collect_floating_widgets(
    tb_rect: &RECT,
    tray_left: i32,
    own_hwnd: HWND,
) -> Vec<(i32, i32)> {
    struct State {
        regions: Vec<(i32, i32)>,
        tb_top: i32, tb_bottom: i32, tb_left: i32,
        tray_left: i32, own: HWND,
    }

    unsafe extern "system" fn cb(hwnd: HWND, lp: LPARAM) -> BOOL {
        let s = &mut *(lp.0 as *mut State);

        // 跳过自身、不可见窗口
        if hwnd == s.own || !IsWindowVisible(hwnd).as_bool() {
            return true.into();
        }

        // 只关注置顶窗口（与任务栏同层的浮动工具）
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        if ex & WS_EX_TOPMOST.0 == 0 { return true.into(); }

        // ⚠️ 跳过已知系统窗口类（尤其是 Shell_TrayWnd 本身！）
        // Shell_TrayWnd 是 HWND_TOPMOST 且横跨整个任务栏宽度，
        // 不过滤的话会把 [taskbar.left, tray_left] 整段都标为占用。
        let mut buf = [0u16; 256];
        let n = GetClassNameW(hwnd, &mut buf);
        if n > 0 {
            let cls = String::from_utf16_lossy(&buf[..n as usize]);
            if is_system_window_class(&cls) { return true.into(); }
        }

        let mut r = RECT::default();
        if GetWindowRect(hwnd, &mut r).is_err() { return true.into(); }

        // ── 垂直「包含」判定（关键修复）────────────────────────────────────
        //
        // 之前用「任意重叠」：只要 bottom > tb_top && top < tb_bottom 就算命中。
        // 这会错误地把搜狗/百度等输入法悬浮框计入（它们出现在屏幕底部时，
        // 窗口底部会稍微碰到任务栏顶部，产生几像素重叠）。
        //
        // 改为「基本包含」：要求窗口的 top 和 bottom 均在任务栏高度范围内
        // （允许 ±4px 容差），确保只有刻意贴着任务栏高度摆放的 widget
        // （如 TrafficMonitor 浮动模式）才被算作占用。
        let tolerance = 4_i32;
        let contained_vertically =
            r.top    >= s.tb_top    - tolerance &&
            r.bottom <= s.tb_bottom + tolerance;
        if !contained_vertically {
            // 打印原因，便于排查哪个窗口触发了过滤
            let mut buf = [0u16; 256];
            let n = GetClassNameW(hwnd, &mut buf);
            let cls = if n > 0 { String::from_utf16_lossy(&buf[..n as usize]) } else { "?".to_string() };
            eprintln!(
                "[taskbar] 跳过垂直超出任务栏的窗口 class={} rect=[{},{},{},{}] taskbar=[{},{}]",
                cls, r.left, r.top, r.right, r.bottom, s.tb_top, s.tb_bottom
            );
            return true.into();
        }

        // 在托盘左侧、任务栏范围内的水平区域
        if r.right <= s.tb_left || r.left >= s.tray_left { return true.into(); }

        let left  = r.left.max(s.tb_left);
        let right = r.right.min(s.tray_left);
        if right > left + 4 {
            let mut buf = [0u16; 256];
            let n = GetClassNameW(hwnd, &mut buf);
            let cls = if n > 0 { String::from_utf16_lossy(&buf[..n as usize]) } else { "?".to_string() };
            eprintln!("[taskbar] 浮动窗口占用 class={} [{}, {}]", cls, left, right);
            s.regions.push((left, right));
        }
        true.into()
    }

    let mut state = State {
        regions:   Vec::new(),
        tb_top:    tb_rect.top,
        tb_bottom: tb_rect.bottom,
        tb_left:   tb_rect.left,
        tray_left,
        own:       own_hwnd,
    };

    let _ = EnumWindows(Some(cb), LPARAM(&mut state as *mut State as isize));
    state.regions
}

// ─── 辅助函数 ────────────────────────────────────────────────────────────────

/// 判断是否为 Windows Shell 系统窗口类或输入法相关窗口（需跳过，不视为第三方占用）
///
/// ## 为什么需要过滤输入法窗口
/// 输入法候选框（如微软拼音、搜狗、百度等）通常是 `WS_EX_TOPMOST` 的浮动窗口。
/// 当用户在屏幕底部的输入框输入文字时，候选框可能出现在任务栏附近，
/// 若不过滤则会被 `collect_floating_widgets` 误判为「占用区间」，导致 widget 跳位。
fn is_system_window_class(cls: &str) -> bool {
    const SYSTEM: &[&str] = &[
        // ── Windows Shell 核心窗口 ──────────────────────────────────────────
        "Shell_TrayWnd",
        "TrayNotifyWnd",
        "ReBarWindow32",
        "MSTaskSwWClass",
        "MSTaskListWClass",
        "TrayDummySearchControl",
        "TrayShowDesktopButtonWClass",
        "TrayButton",
        "Button",
        "SysPager",
        "ToolbarWindow32",
        "NotifyIconOverflowWindow",
        "WorkerW",
        "Progman",
        "DV2ControlHost",
        "Shell_SecondaryTrayWnd",
        "Clock",
        // ── 输入法框架（Text Services Framework）───────────────────────────
        // 微软 TSF 核心
        "MSCTFIME UI",
        "IME",
        "CiceroUIWndFrame",          // TSF 语言栏浮动框架
        "TF_FloatLangBar_WndTitle",   // TSF 语言栏标题
        "TF_CiceroHiddenWnd",         // TSF 隐藏辅助窗口
        "TfThreadUI",                 // TSF 线程 UI
        // 微软拼音 / 微软输入法
        "Microsoft IME",
        "MSTIP_DocIcon",
        // ── 第三方主流输入法（防止候选框被误计入占用区间）──────────────────
        // 搜狗输入法
        "SogouPY",
        "SogouPY.1",
        "SogouPYCandWindow",
        // 百度输入法
        "BaiduIMEWndClass",
        "BaiduPinyin",
        // 讯飞输入法
        "IFLYIMEWndClass",
        // 谷歌拼音
        "GooglePinyinInputMain",
        // QQ 输入法
        "QQInputMain",
        // 紫光拼音
        "SPInputMain",
    ];

    SYSTEM.iter().any(|&s| cls == s)
        || cls.starts_with("Windows.UI.")
        || cls.starts_with("Windows.Internal.")
        || cls.contains("XamlIsland")
        || cls.contains("Xaml")
        // TSF / IME 相关类名通配
        || cls.starts_with("MSCTF")
        || cls.starts_with("TF_")
        || cls.starts_with("Sogou")
        || cls.starts_with("Baidu")
        || cls.starts_with("IFLY")
        || cls.ends_with("IMEWndClass")
        || cls.ends_with("InputMain")
        || cls.ends_with("CandWindow")
        || cls.ends_with("CandidateWnd")
}

/// 从 `start_x` 向左找第一个宽度为 `width` 的空闲区间，不与任何 `occupied` 重叠。
///
/// # 算法
/// 候选位置 = {start_x} ∪ {每个占用区间的 left − width − 4}
/// 从最右侧候选位置依次尝试，返回第一个完全空闲的；最小值为 `min_x`。
///
/// **正确性保证**：候选集覆盖了所有最优停靠点——如果某候选被遮挡，
/// 则恰好遮挡它的那个区间的 left 也在候选集中，最终一定能找到空闲位置。

fn find_free_x(start_x: i32, width: i32, occupied: &[(i32, i32)], min_x: i32) -> i32 {
    // 生成候选位置：start_x + 每个区间 left 左移一个 widget 宽度
    let mut candidates: Vec<i32> = std::iter::once(start_x)
        .chain(occupied.iter().map(|&(l, _)| l - width - 4))
        .filter(|&x| x >= min_x)
        .collect();

    // 从右到左依次尝试（优先靠近托盘的位置）
    candidates.sort_unstable_by(|a, b| b.cmp(a));
    candidates.dedup();

    for x in &candidates {
        let x_end = x + width;
        let is_free = occupied.iter().all(|&(l, r)| x_end <= l || *x >= r);
        if is_free {
            eprintln!("[taskbar] 找到空闲位置: {}", x);
            return *x;
        }
    }

    // 所有候选都被占用，退回到最小边界
    eprintln!("[taskbar] 所有候选位置被占用，回退到 min_x={}", min_x);
    min_x
}

/// 字符串转以 null 结尾的 UTF-16 向量（供 Win32 API 使用）

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

// ─── 数据更新 ────────────────────────────────────────────────────────────────

/// 接收 Provider 数据，更新显示文字并触发重绘

/// 用最新数据更新任务栏组件显示内容并触发重绘。
///
/// 直接使用各 Provider 已预格式化的 `compact_text` 字段，
/// 按全角冒号 `：` 分割为两行：
///   - 冒号前：行1（如 "5h剩余"、"近5H"）
///   - 冒号后：行2（如 "1234k"、"85.1%"）
/// 若无冒号则仅显示一行居中文字（如 "获取数据..."）。
///
/// 重绘通过 `PostMessageW(WM_APP)` 发送到窗口线程的消息队列，
/// 而非直接调用 `InvalidateRect`，确保跨线程安全。
pub fn update_widget_data(data: &serde_json::Value) {
    // Line1：使用 provider_name（当前激活 TAB 的名称）
    let provider_name = data
        .get("provider_name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    // Line2：compact_text 中「：」后的数值部分（全角冒号分隔）
    let compact = data
        .get("compact_text")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let value_str = if let Some(pos) = compact.find('\u{FF1A}') {
        // "5h剩余：1234k" → "1234k"
        compact[pos + '\u{FF1A}'.len_utf8()..].to_string()
    } else if compact.is_empty() {
        "加载中...".to_string()
    } else {
        // 无冒号（如 "获取数据..."）→ 整行作为 Line2
        compact.to_string()
    };

    let (line1, line2) = if provider_name.is_empty() {
        ("".to_string(), value_str)
    } else {
        (provider_name, value_str)
    };

    if let Ok(mut g) = QUOTA_TEXT.lock() {
        *g = Some((line1.clone(), line2.clone()));
    }

    // 跨线程安全触发重绘：PostMessageW 将 WM_APP 消息投入窗口线程的消息队列，
    // 由窗口线程自身执行 InvalidateRect + UpdateWindow，避免跨线程直接调用 GDI。
    let ptr = WIDGET_HWND.load(Ordering::Relaxed);
    if !ptr.is_null() {
        unsafe {
            let hwnd = HWND(ptr);
            let _ = PostMessageW(hwnd, WM_APP, WPARAM(0), LPARAM(0));
        }
        eprintln!("[taskbar] 数据已更新 → '{}' / '{}'", line1, line2);
    }
}

// ─── 非 Windows 平台空实现 ────────────────────────────────────────────────────

#[cfg(not(target_os = "windows"))]
pub fn create_taskbar_widget(_handle: &AppHandle) -> tauri::Result<()> {
    Ok(())
}

#[cfg(not(target_os = "windows"))]
pub fn setup_taskbar_widget_events(_handle: &AppHandle) {}

#[cfg(not(target_os = "windows"))]
pub fn show_taskbar_widget() {}
