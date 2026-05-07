//! macOS 托盘扩展模块
//!
//! 负责：
//! - 保存托盘图标引用（供跨模块调用）
//! - 注册左键点击事件（切换悬浮窗）
//! - 根据 overlay_active_tab_changed 事件更新托盘标题

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tauri::{AppHandle, Manager, Wry};

// ─── 全局托盘引用 ─────────────────────────────────────────────────────────────

/// 存储托盘图标引用，供 update_active_tab_tray 使用
static MAIN_TRAY: Mutex<Option<tauri::tray::TrayIcon<Wry>>> = Mutex::new(None);

// ─── 公共接口 ─────────────────────────────────────────────────────────────────

/// 保存托盘图标引用（由 tray::setup_tray 调用）
pub fn register_tray(tray: tauri::tray::TrayIcon<Wry>) {
    if let Ok(mut g) = MAIN_TRAY.lock() {
        *g = Some(tray);
    }
}

/// 由 overlay_active_tab_changed 事件驱动：更新 macOS 托盘标题
///
/// 格式（多 TAB 时）：" ₁₎₁₂₃₄ₖ"  — 下标序号 + 下标右括号 + 下标数值
/// 格式（单 TAB 时）：" ₁₂₃₄ₖ"    — 仅下标数值，不显示序号前缀
pub fn update_active_tab_tray(data: &serde_json::Value) {
    let guard = match MAIN_TRAY.lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    if let Some(tray) = guard.as_ref() {
        update_tray_title_for_active(tray, data);
    }
}

/// 注册左键点击托盘图标事件（切换悬浮窗显示/隐藏）
///
/// 内置 300 ms 防抖，避免按下与释放都触发。
pub fn setup_click_handler(handle: &AppHandle, tray: &tauri::tray::TrayIcon<Wry>) {
    let app_click = handle.clone();
    let last_click = Arc::new(Mutex::new(Instant::now() - Duration::from_secs(1)));

    tray.on_tray_icon_event(move |_tray, event| {
        if let tauri::tray::TrayIconEvent::Click {
            button,
            button_state,
            ..
        } = event
        {
            if button == tauri::tray::MouseButton::Left
                && button_state == tauri::tray::MouseButtonState::Up
            {
                // 防抖：距上次点击不足 300ms 则忽略
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

// ─── 私有实现 ─────────────────────────────────────────────────────────────────

/// 根据 overlay_active_tab_changed 数据更新托盘标题
fn update_tray_title_for_active(tray: &tauri::tray::TrayIcon<Wry>, data: &serde_json::Value) {
    let tab_index = data.get("tab_index").and_then(|v| v.as_u64()).unwrap_or(1);
    let tab_count = data.get("tab_count").and_then(|v| v.as_u64()).unwrap_or(1);

    // 多 TAB：前缀 = 下标序号 + 下标右括号 ₎ (U+208E)，如 ₁₎  ₂₎
    // 单 TAB：无前缀，仅显示数值
    let prefix = if tab_count > 1 {
        to_subscript_with_paren(tab_index)
    } else {
        String::new()
    };

    // ── 优先从 quota_groups[0] 提取数值 ──────────────────────────────────
    if let Some(quota_groups) = data.get("quota_groups").and_then(|v| v.as_array()) {
        if let Some(first) = quota_groups.first() {
            let limit = first.get("limit").and_then(|v| v.as_u64()).unwrap_or(0);

            // 情况1：有具体 token 限额 -> 显示剩余量
            if limit > 0 {
                let remain = first.get("remain").and_then(|v| v.as_u64()).unwrap_or(0);
                let remain_str = if remain >= 10000 {
                    format!("{}k", remain / 1000)
                } else {
                    remain.to_string()
                };
                let title = format!(" {}{}", prefix, to_subscript(&remain_str));
                let _ = tray.set_title(Some(&title));
                eprintln!("[tray] title -> '{}' (remain)", title);
                return;
            }

            // 情况2：只有百分比（火山引擎）-> 四舍五入取整后显示
            if let Some(used_str) = first.get("used").and_then(|v| v.as_str()) {
                let pct_str = used_str.trim_end_matches('%');
                if let Ok(pct_f) = pct_str.parse::<f64>() {
                    let pct_i = pct_f.round() as i64;
                    let title = format!(" {}{}", prefix, to_subscript(&pct_i.to_string()));
                    let _ = tray.set_title(Some(&title));
                    eprintln!("[tray] title -> '{}' (pct)", title);
                    return;
                }
            }
        }
    }

    // ── 回退：解析 compact_text 中全角冒号「：」后的数值部分 ──────────────
    let compact = data.get("compact_text").and_then(|v| v.as_str()).unwrap_or("");
    if let Some(pos) = compact.find('\u{FF1A}') {
        let value = &compact[pos + '\u{FF1A}'.len_utf8()..];
        if !value.is_empty() {
            let title = format!(" {}{}", prefix, to_subscript(value));
            let _ = tray.set_title(Some(&title));
            eprintln!("[tray] title -> '{}' (compact_text)", title);
            return;
        }
    }

    // ── 数据尚未就绪（如"获取数据..."）────────────────────────────────────
    if tab_count > 1 {
        // 多 TAB：至少显示序号前缀，让用户知道当前激活的是哪个 TAB
        let title = format!(" {}", prefix);
        let _ = tray.set_title(Some(&title));
        eprintln!("[tray] title -> '{}' (no data, multi-tab)", title);
    } else {
        // 单 TAB：数据未就绪时清空标题
        let _ = tray.set_title(Option::<&str>::None);
        eprintln!("[tray] title cleared (no data, single tab)");
    }
}

/// 将 TAB 序号（u64）转换为「下标数字 + 下标右括号」前缀
/// 例如：1 → "₁₎"，2 → "₂₎"，12 → "₁₂₎"
fn to_subscript_with_paren(n: u64) -> String {
    let mut s = to_subscript(&n.to_string());
    s.push('\u{208E}'); // ₎ SUBSCRIPT RIGHT PARENTHESIS
    s
}

/// 将数字/字母字符串转换为 Unicode 下标字符
fn to_subscript(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '0' => '\u{2080}', // ₀
            '1' => '\u{2081}', // ₁
            '2' => '\u{2082}', // ₂
            '3' => '\u{2083}', // ₃
            '4' => '\u{2084}', // ₄
            '5' => '\u{2085}', // ₅
            '6' => '\u{2086}', // ₆
            '7' => '\u{2087}', // ₇
            '8' => '\u{2088}', // ₈
            '9' => '\u{2089}', // ₉
            'k' => '\u{2096}', // ₖ
            'w' => 'w',        // w 没有标准下标，保持原字符
            '%' => '%',
            '.' => '.',
            other => other,
        })
        .collect()
}
