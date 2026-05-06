//! 百度千帆 Provider
//!
//! 登录入口：直接打开千帆控制台（未登录时会自动跳到百度登录页，登录后跳回）
//! 目标页面：千帆 > 资源 > 我的订阅
//! 数据：资源包名称、状态、Token 配额与剩余、到期日期

use super::ProviderConfig;

pub fn provider() -> ProviderConfig {
    ProviderConfig {
        id: "baidu",
        name: "百度千帆",
        target_url: "https://console.bce.baidu.com/qianfan/resource/subscribe",
        allowed_domains: vec![
            "https://*.bce.baidu.com".to_string(),
            "https://bce.baidu.com".to_string(),
            // 百度登录域（登录页上的 IPC 调用会静默失败，无安全风险）
            "https://*.baidu.com".to_string(),
        ],
        injection_script: injection_script(),
    }
}

fn injection_script() -> String {
    r#"
(function () {
    if (window.__LSYS_BAIDU_INJECTED__) return;
    window.__LSYS_BAIDU_INJECTED__ = true;

    const PROVIDER_ID    = 'baidu';
    const PROVIDER_NAME  = '百度千帆';
    let FETCH_INTERVAL = 60_000; // 默认60秒，可通过 __LSYS_SET_INTERVAL__ 动态修改

    let prevLoggedIn = null;
    let lastFetchAt  = 0;
    let checkPaused  = false; // 检测是否暂停

    // 暴露全局函数供 Rust 侧动态修改刷新间隔
    window.__LSYS_SET_INTERVAL__ = function(intervalMs) {
        FETCH_INTERVAL = intervalMs;
        console.log('[Baidu] 刷新间隔已更新为:', intervalMs, 'ms');
    };

    // 暴露全局函数供 Rust 侧暂停/恢复检测
    window.__LSYS_PAUSE_CHECK__ = function() {
        checkPaused = true;
        console.log('[Baidu] 登录检测已暂停');
    };

    window.__LSYS_RESUME_CHECK__ = function() {
        checkPaused = false;
        console.log('[Baidu] 登录检测已恢复');
    };

    // ── IPC 工具 ───────────────────────────────────────────────────────────────

    async function emitToRust(eventName, payload) {
        try {
            if (window.__TAURI__?.event?.emit) {
                await window.__TAURI__.event.emit(eventName, payload);
            }
        } catch (e) {
            console.error('[Baidu] emit failed:', eventName, e);
        }
    }

    // ── Cookie 工具 ────────────────────────────────────────────────────────────

    function getCookie(name) {
        for (const part of document.cookie.split(';')) {
            const [k, ...v] = part.trim().split('=');
            if (k.trim() === name) return v.join('=') || '';
        }
        return '';
    }

    function isLoggedIn() {
        const accountId     = getCookie('bce-login-accountid');
        const domainAccount = getCookie('bce-login-domain-account');
        return accountId !== '' && domainAccount !== '';
    }

    // ── 格式化工具 ─────────────────────────────────────────────────────────────

    function formatNum(n) {
        if (n >= 10000) return (n / 10000).toFixed(1) + 'w';
        return String(n);
    }

    function formatDate(iso) {
        try {
            const d = new Date(iso);
            const p = n => String(n).padStart(2, '0');
            return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`;
        } catch (_) {
            return iso || '';
        }
    }

    // ── 数据构建 ───────────────────────────────────────────────────────────────

    function buildProviderData(item) {
        const quota    = item.quota     || {};
        const fiveHour = quota.fiveHour || {};
        const week     = quota.week     || {};
        const month    = quota.month    || {};

        const fiveUsed   = fiveHour.used  || 0;
        const fiveLimit  = fiveHour.limit || 0;
        const fiveRemain = fiveLimit - fiveUsed;

        const weekUsed   = week.used  || 0;
        const weekLimit  = week.limit || 0;
        const weekRemain = weekLimit - weekUsed;

        const monthUsed   = month.used  || 0;
        const monthLimit  = month.limit || 0;
        const monthRemain = monthLimit - monthUsed;

        const items = [
            { key: '套餐', value: item.planType       || '', highlight: false },
            { key: '状态', value: item.resourceStatus || '', highlight: true  },
            { key: '到期', value: formatDate(item.expiresAt),highlight: false },
        ];

        const quota_groups = [
            { label: '近5小时', used: fiveUsed,  limit: fiveLimit,  remain: fiveRemain,  highlight: fiveRemain  < 200  },
            { label: '近一周',  used: weekUsed,  limit: weekLimit,  remain: weekRemain,  highlight: weekRemain  < 1000 },
            { label: '近一月',  used: monthUsed, limit: monthLimit, remain: monthRemain, highlight: monthRemain < 2000 },
        ];

        const compact_text = fiveLimit > 0
            ? `5h剩余：${formatNum(fiveRemain)}`
            : (item.resourceStatus || '百度千帆');

        return { provider_id: PROVIDER_ID, provider_name: PROVIDER_NAME, items, quota_groups, updated_at: Date.now(), compact_text };
    }

    // ── 数据拉取 ───────────────────────────────────────────────────────────────

    async function fetchAndEmitData() {
        try {
            const resp = await fetch(
                'https://console.bce.baidu.com/api/qianfan/charge/codingPlan/resourceList',
                { credentials: 'include' }
            );
            if (!resp.ok) return; // 失败：不更新 lastFetchAt → 下次 tick 重试
            const data = await resp.json();
            if (!data.success || !data.result?.items?.length) return;
            await emitToRust('provider_data_updated', buildProviderData(data.result.items[0]));
            lastFetchAt = Date.now(); // 仅在成功推送后更新，控制 60s 刷新周期
        } catch (e) {
            console.error('[Baidu] fetchAndEmitData failed:', e);
            // 异常时不更新 lastFetchAt → 下次 tick 立即重试
        }
    }

    // ── 主循环 ─────────────────────────────────────────────────────────────────

    async function tick() {
        // 如果检测被暂停，跳过本次检测
        if (checkPaused) {
            console.log('[Baidu] 检测已暂停，跳过本次 tick');
            return;
        }

        const loggedIn = isLoggedIn();

        if (prevLoggedIn !== true && loggedIn) {
            // null/false → true：检测到登录
            prevLoggedIn = true;
            await emitToRust('provider_login_detected', { provider_id: PROVIDER_ID });
            // Rust 侧收到后会立即推占位数据显示悬浮窗。
            // lastFetchAt 保持 0 → 下方区间检查立即为 true → 本 tick 末尾立即拉取真实数据；
            // 若失败，lastFetchAt 不更新，每 2s tick 持续重试直到成功。
        } else if (prevLoggedIn === true && !loggedIn) {
            // true → false：检测到登出
            prevLoggedIn = false;
            await emitToRust('provider_logout_detected', { provider_id: PROVIDER_ID });
        } else if (prevLoggedIn === null) {
            // 初始化：当前未登录，静默设置
            prevLoggedIn = false;
        }

        // 已登录时按间隔拉取数据。
        // lastFetchAt=0 时 Date.now()-0 >> FETCH_INTERVAL，立即触发；
        // 成功后 fetchAndEmitData 内部更新 lastFetchAt，60s 内不重复拉取。
        if (loggedIn && Date.now() - lastFetchAt >= FETCH_INTERVAL) {
            await fetchAndEmitData();
        }
    }

    setInterval(tick, 2000);
    window.addEventListener('load', () => tick());
})();
    "#.to_string()
}
