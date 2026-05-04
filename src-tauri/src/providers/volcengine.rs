//! 火山引擎方舟 Provider
//!
//! 登录入口：直接打开方舟控制台（未登录时会自动跳到登录页，登录后跳回）
//! 目标页面：方舟 > 开放管理 > 订阅
//! 登录判定：Cookie 中 AccountID 存在且非空
//! 数据来源：
//!   - ListSubscribeTrade 接口：获取套餐订阅信息（套餐类型、状态、有效期等）
//!   - GetCodingPlanUsage 接口：获取用量百分比（session / weekly / monthly）

use super::ProviderConfig;

pub fn provider() -> ProviderConfig {
    ProviderConfig {
        id: "volcengine",
        name: "火山引擎方舟",
        target_url: "https://console.volcengine.com/ark/region:ark+cn-beijing/openManagement?LLM=%7B%7D&advancedActiveKey=subscribe",
        allowed_domains: vec![
            "https://*.volcengine.com".to_string(),
            "https://volcengine.com".to_string(),
        ],
        injection_script: injection_script(),
    }
}

fn injection_script() -> String {
    r#"
(function () {
    console.log('[Volcengine] 注入脚本开始执行');
    console.log('[Volcengine] 当前 URL:', window.location.href);
    console.log('[Volcengine] Tauri API 可用性:', typeof window.__TAURI__);

    if (window.__LSYS_VOLCENGINE_INJECTED__) {
        console.log('[Volcengine] 脚本已注入，跳过');
        return;
    }
    window.__LSYS_VOLCENGINE_INJECTED__ = true;

    const PROVIDER_ID    = 'volcengine';
    const PROVIDER_NAME  = '火山引擎方舟';
    let FETCH_INTERVAL = 60_000; // 默认60秒，可通过 __LSYS_SET_INTERVAL__ 动态修改

    let prevLoggedIn = null; // null | true | false
    let lastFetchAt  = 0;

    // 暴露全局函数供 Rust 侧动态修改刷新间隔
    window.__LSYS_SET_INTERVAL__ = function(intervalMs) {
        FETCH_INTERVAL = intervalMs;
        console.log('[Volcengine] 刷新间隔已更新为:', intervalMs, 'ms');
    };

    // ── IPC 工具 ──────────────────────────────────────────────────────────────

    async function emitToRust(eventName, payload) {
        try {
            console.log('[Volcengine] 尝试发送事件:', eventName, JSON.stringify(payload).substring(0, 200));
            if (window.__TAURI__?.event?.emit) {
                await window.__TAURI__.event.emit(eventName, payload);
                console.log('[Volcengine] 事件发送成功:', eventName);
            } else {
                console.error('[Volcengine] Tauri API 不可用, __TAURI__:', typeof window.__TAURI__);
            }
        } catch (e) {
            console.error('[Volcengine] emit failed:', eventName, e);
        }
    }

    // ── Cookie 工具 ───────────────────────────────────────────────────────────

    function getCookie(name) {
        for (const part of document.cookie.split(';')) {
            const [k, ...v] = part.trim().split('=');
            if (k.trim() === name) return v.join('=') || '';
        }
        return '';
    }

    function isLoggedIn() {
        const accountId = getCookie('AccountID');
        return accountId !== '';
    }

    // ── 获取 CSRF Token ──────────────────────────────────────────────────────
    // 火山引擎控制台 API（TOP 网关）可能需要 CSRF token

    function getCsrfToken() {
        // 1. 尝试从 cookie 获取
        const csrfCookie = getCookie('csrfToken') || getCookie('csrf_token') || getCookie('CSRF_TOKEN');
        if (csrfCookie) return csrfCookie;
        // 2. 尝试从 meta 标签获取
        const metaEl = document.querySelector('meta[name="csrf-token"]')
                    || document.querySelector('meta[name="csrfToken"]');
        if (metaEl) return metaEl.getAttribute('content') || '';
        return '';
    }

    // 构建请求头：包含 CSRF token 等火山引擎可能需要的头
    function buildHeaders() {
        const headers = { 'Content-Type': 'application/json' };
        const csrf = getCsrfToken();
        if (csrf) {
            headers['x-csrf-token'] = csrf;
            console.log('[Volcengine] 已添加 CSRF token');
        }
        const accountId = getCookie('AccountID');
        if (accountId) {
            headers['x-top-account-id'] = accountId;
        }
        return headers;
    }

    // ── 格式化工具 ────────────────────────────────────────────────────────────

    function formatDate(iso) {
        try {
            const d = new Date(iso);
            const p = n => String(n).padStart(2, '0');
            return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`;
        } catch (_) {
            return iso || '';
        }
    }

    function formatTimestamp(ts) {
        if (!ts || ts < 0) return '--';
        try {
            return formatDate(new Date(ts * 1000).toISOString());
        } catch (_) {
            return '--';
        }
    }

    function formatPercent(p) {
        if (typeof p !== 'number') return '--';
        return (p * 100).toFixed(1) + '%';
    }

    // 将 BizInfo 映射为可读套餐名
    function bizInfoToName(bizInfo) {
        const map = { 'lite': 'Lite', 'pro': 'Pro', 'enterprise': 'Enterprise' };
        return map[bizInfo] || bizInfo || '未知';
    }

    // 将 Status 映射为可读状态
    function statusToText(status) {
        const map = {
            'Running':   '运行中',
            'Stopped':   '已停止',
            'Expired':   '已过期',
            'Pending':   '待生效',
        };
        return map[status] || status || '--';
    }

    // ── 数据构建 ──────────────────────────────────────────────────────────────

    function buildProviderData(tradeInfo, usageInfo) {
        const items = [];
        const quota_groups = [];

        // ── 套餐信息（来自 ListSubscribeTrade）──
        if (tradeInfo) {
            const bizName = bizInfoToName(tradeInfo.BizInfo);
            const statusText = statusToText(tradeInfo.Status);

            items.push({ key: '套餐', value: bizName, highlight: false });
            items.push({ key: '状态', value: statusText, highlight: tradeInfo.Status === 'Running' });

            if (tradeInfo.EndTime) {
                items.push({ key: '到期', value: formatDate(tradeInfo.EndTime), highlight: false });
            }

            if (tradeInfo.EnableAutoRenew !== undefined) {
                items.push({ key: '自动续费', value: tradeInfo.EnableAutoRenew ? '是' : '否', highlight: false });
            }
        }

        // ── 用量信息（来自 GetCodingPlanUsage）──
        if (usageInfo && usageInfo.QuotaUsage) {
            if (usageInfo.UpdateTimestamp) {
                items.push({ key: '统计时间', value: formatTimestamp(usageInfo.UpdateTimestamp), highlight: false });
            }

            console.log('[Volcengine] QuotaUsage 数组:', JSON.stringify(usageInfo.QuotaUsage));

            for (const q of usageInfo.QuotaUsage) {
                console.log('[Volcengine] 处理配额项:', JSON.stringify(q));
                let label = '';
                let highlightThreshold = 1;

                if (q.Level === 'session') {
                    label = '近5小时';
                    highlightThreshold = 0.8;
                    console.log('[Volcengine] 找到 session 级别，标签设为: 近5小时');
                } else if (q.Level === 'weekly') {
                    label = '近一周';
                    highlightThreshold = 0.8;
                } else if (q.Level === 'monthly') {
                    label = '近一月';
                    highlightThreshold = 0.8;
                } else {
                    label = q.Level || '未知';
                    console.log('[Volcengine] 未知级别:', q.Level);
                }

                // API 返回的 Percent 已是百分比值（如 85.5 表示 85.5%），需除以 100 归一化为小数
                const percent = typeof q.Percent === 'number' ? q.Percent / 100 : 0;
                const resetText = q.ResetTimestamp > 0
                    ? '重置: ' + formatTimestamp(q.ResetTimestamp)
                    : '';

                const quotaItem = {
                    label: label,
                    used:  formatPercent(percent),
                    limit: 0,
                    remain: resetText,
                    highlight: percent >= highlightThreshold,
                };
                console.log('[Volcengine] 添加配额项:', JSON.stringify(quotaItem));
                quota_groups.push(quotaItem);
            }
        }

        // ── 折叠摘要 ──
        let compact_text = '火山引擎方舟';
        if (usageInfo && usageInfo.QuotaUsage) {
            console.log('[Volcengine] 构建 compact_text，QuotaUsage 长度:', usageInfo.QuotaUsage.length);
            // 优先显示5小时用量（session），其次显示周用量
            const session = usageInfo.QuotaUsage.find(q => q.Level === 'session');
            const weekly = usageInfo.QuotaUsage.find(q => q.Level === 'weekly');
            console.log('[Volcengine] session 数据:', session ? JSON.stringify(session) : 'null');
            console.log('[Volcengine] weekly 数据:', weekly ? JSON.stringify(weekly) : 'null');
            if (session) {
                compact_text = '5h用量：' + formatPercent(session.Percent / 100);
                console.log('[Volcengine] compact_text 设为:', compact_text);
            } else if (weekly) {
                compact_text = '周用量：' + formatPercent(weekly.Percent / 100);
                console.log('[Volcengine] compact_text 设为:', compact_text);
            }
        }
        if (tradeInfo && tradeInfo.Status && tradeInfo.Status !== 'Running') {
            compact_text = '状态：' + statusToText(tradeInfo.Status);
        }
        console.log('[Volcengine] 最终 compact_text:', compact_text);

        return {
            provider_id:   PROVIDER_ID,
            provider_name: PROVIDER_NAME,
            items,
            quota_groups,
            updated_at: Date.now(),
            compact_text,
        };
    }

    // ── 数据拉取 ──────────────────────────────────────────────────────────────

    async function fetchAndEmitData() {
        console.log('[Volcengine] fetchAndEmitData 开始执行');
        console.log('[Volcengine] 当前 cookies (前200字符):', document.cookie.substring(0, 200));

        const headers = buildHeaders();
        console.log('[Volcengine] 请求头:', JSON.stringify(headers));

        try {
            // 1. 获取套餐订阅信息
            let tradeInfo = null;
            try {
                console.log('[Volcengine] 正在请求 ListSubscribeTrade...');
                const tradeResp = await fetch(
                    'https://console.volcengine.com/api/top/ark/cn-beijing/2024-01-01/ListSubscribeTrade?',
                    {
                        method: 'POST',
                        credentials: 'include',
                        headers: headers,
                        body: JSON.stringify({
                            ResourceTypes: ['CodingPlan'],
                            ResourceNames: [''],
                            BizInfos: ['lite', 'pro'],
                        }),
                    }
                );
                console.log('[Volcengine] ListSubscribeTrade 响应状态:', tradeResp.status, tradeResp.statusText);
                const tradeText = await tradeResp.text();
                console.log('[Volcengine] ListSubscribeTrade 响应体 (前500字符):', tradeText.substring(0, 500));

                if (tradeResp.ok) {
                    const tradeData = JSON.parse(tradeText);
                    const infoList = tradeData?.Result?.InfoList;
                    console.log('[Volcengine] InfoList 长度:', infoList?.length || 0);
                    if (Array.isArray(infoList) && infoList.length > 0) {
                        // 优先选取 Running 状态的套餐；若无则取第一条
                        tradeInfo = infoList.find(i => i.Status === 'Running') || infoList[0];
                        console.log('[Volcengine] 选取的套餐:', JSON.stringify(tradeInfo).substring(0, 300));
                    }
                } else {
                    console.error('[Volcengine] ListSubscribeTrade 请求失败:', tradeResp.status);
                }
            } catch (e) {
                console.error('[Volcengine] ListSubscribeTrade 异常:', e);
            }

            // 2. 获取用量数据
            let usageInfo = null;
            try {
                console.log('[Volcengine] 正在请求 GetCodingPlanUsage...');
                const usageResp = await fetch(
                    'https://console.volcengine.com/api/top/ark/cn-beijing/2024-01-01/GetCodingPlanUsage?',
                    {
                        method: 'POST',
                        credentials: 'include',
                        headers: headers,
                        body: JSON.stringify({}),
                    }
                );
                console.log('[Volcengine] GetCodingPlanUsage 响应状态:', usageResp.status, usageResp.statusText);
                const usageText = await usageResp.text();
                console.log('[Volcengine] GetCodingPlanUsage 响应体 (前500字符):', usageText.substring(0, 500));

                if (usageResp.ok) {
                    const usageData = JSON.parse(usageText);
                    if (usageData?.Result) {
                        usageInfo = usageData.Result;
                        console.log('[Volcengine] 用量数据:', JSON.stringify(usageInfo).substring(0, 300));
                    }
                } else {
                    console.error('[Volcengine] GetCodingPlanUsage 请求失败:', usageResp.status);
                }
            } catch (e) {
                console.error('[Volcengine] GetCodingPlanUsage 异常:', e);
            }

            // 至少有一个接口成功才推送数据
            if (!tradeInfo && !usageInfo) {
                console.warn('[Volcengine] 两个接口均未获取到有效数据，跳过推送');
                return;
            }

            const payload = buildProviderData(tradeInfo, usageInfo);
            console.log('[Volcengine] 准备推送数据:', JSON.stringify(payload).substring(0, 300));
            await emitToRust('provider_data_updated', payload);
            lastFetchAt = Date.now(); // 仅在成功推送后更新，控制刷新周期
            console.log('[Volcengine] 数据推送成功, lastFetchAt 已更新');
        } catch (e) {
            console.error('[Volcengine] fetchAndEmitData 顶层异常:', e);
            // 异常时不更新 lastFetchAt → 下次 tick 立即重试
        }
    }

    // ── 主循环 ────────────────────────────────────────────────────────────────

    async function tick() {
        const loggedIn = isLoggedIn();

        if (prevLoggedIn !== true && loggedIn) {
            // null/false → true：检测到登录
            console.log('[Volcengine] 检测到登录, AccountID:', getCookie('AccountID').substring(0, 10) + '...');
            prevLoggedIn = true;
            await emitToRust('provider_login_detected', { provider_id: PROVIDER_ID });
        } else if (prevLoggedIn === true && !loggedIn) {
            // true → false：检测到登出
            console.log('[Volcengine] 检测到登出');
            prevLoggedIn = false;
            await emitToRust('provider_logout_detected', { provider_id: PROVIDER_ID });
        } else if (prevLoggedIn === null) {
            // 初始化：当前未登录，静默设置
            prevLoggedIn = false;
        }

        // 已登录时按间隔拉取数据
        if (loggedIn && Date.now() - lastFetchAt >= FETCH_INTERVAL) {
            console.log('[Volcengine] 触发数据拉取, 距上次:', Date.now() - lastFetchAt, 'ms');
            await fetchAndEmitData();
        }
    }

    console.log('[Volcengine] 启动定时器');
    setInterval(tick, 2000);
    window.addEventListener('load', () => {
        console.log('[Volcengine] 页面加载完成，执行 tick');
        tick();
    });

    // 立即执行一次
    console.log('[Volcengine] 立即执行首次 tick');
    tick();
})();
    "#.to_string()
}
