//! 阿里云百炼 Provider
//!
//! 直接加载百炼控制台目标页，未登录时页面自动弹出登录框。
//! 登录成功后 Cookie 写入，注入脚本检测到后上报并开始采集数据。

use super::ProviderConfig;

pub fn provider() -> ProviderConfig {
    ProviderConfig {
        id: "aliyun",
        name: "阿里云百炼",
        target_url: "https://bailian.console.aliyun.com/cn-beijing?tab=plan#/efm/subscription/coding-plan",
        allowed_domains: vec![
            "https://*.aliyun.com".to_string(),
            "https://aliyun.com".to_string(),
        ],
        injection_script: injection_script(),
    }
}

fn injection_script() -> String {
    r#"
(function () {
    console.log('[Aliyun] 注入脚本开始执行');
    console.log('[Aliyun] 当前 URL:', window.location.href);
    console.log('[Aliyun] Tauri API 可用性:', typeof window.__TAURI__);
    
    if (window.__LSYS_ALIYUN_INJECTED__) {
        console.log('[Aliyun] 脚本已注入，跳过');
        return;
    }
    window.__LSYS_ALIYUN_INJECTED__ = true;

    const PROVIDER_ID    = 'aliyun';
    const PROVIDER_NAME  = '阿里云百炼';
    let FETCH_INTERVAL = 60_000; // 默认60秒，可通过 __LSYS_SET_INTERVAL__ 动态修改

    let prevLoggedIn = null; // null | true | false
    let lastFetchAt  = 0;
    let checkPaused  = false; // 检测是否暂停

    // 暴露全局函数供 Rust 侧动态修改刷新间隔
    window.__LSYS_SET_INTERVAL__ = function(intervalMs) {
        FETCH_INTERVAL = intervalMs;
        console.log('[Aliyun] 刷新间隔已更新为:', intervalMs, 'ms');
    };

    // 暴露全局函数供 Rust 侧暂停/恢复检测
    window.__LSYS_PAUSE_CHECK__ = function() {
        checkPaused = true;
        console.log('[Aliyun] 登录检测已暂停');
    };

    window.__LSYS_RESUME_CHECK__ = function() {
        checkPaused = false;
        console.log('[Aliyun] 登录检测已恢复');
    };

    // ── IPC 工具 ──────────────────────────────────────────────────────────────

    async function emitToRust(eventName, payload) {
        try {
            console.log('[Aliyun] 尝试发送事件:', eventName, payload);
            if (window.__TAURI__?.event?.emit) {
                await window.__TAURI__.event.emit(eventName, payload);
                console.log('[Aliyun] 事件发送成功:', eventName);
            } else {
                console.error('[Aliyun] Tauri API 不可用, __TAURI__:', typeof window.__TAURI__);
            }
        } catch (e) {
            console.error('[Aliyun] emit failed:', eventName, e);
        }
    }

    // ── Cookie 工具 ───────────────────────────────────────────────────────────

    function getCookie(name) {
        for (const part of document.cookie.split(';')) {
            const [k, ...v] = part.trim().split('=');
            if (k === name) return v.join('=') || '';
        }
        return '';
    }

    // ── 数据提取 ──────────────────────────────────────────────────────────────

    function extractPlanInfo() {
        try {
            const lines = document.body.innerText
                .split('\n')
                .map(l => l.trim())
                .filter(Boolean);

            const info = {
                planName: '', status: '', remainingDays: '',
                startTime: '', endTime: '', autoRenew: '',
                lastStatTime: '', usage5h: '', usageWeek: '', usageMonth: '',
            };

            for (let i = 0; i < lines.length; i++) {
                const l    = lines[i];
                const next = lines[i + 1] ?? '';
                if (l === '套餐状态')    info.status        = next;
                if (l === '剩余天数')    info.remainingDays = next;
                if (l === '开始时间')    info.startTime     = next;
                if (l === '结束时间')    info.endTime       = next;
                if (l === '自动续费')    info.autoRenew     = next;
                if (l === '近5小时用量') info.usage5h       = next;
                if (l === '近一周用量')  info.usageWeek     = next;
                if (l === '近一月用量')  info.usageMonth    = next;
                if (l.includes('最后统计时间')) {
                    info.lastStatTime = l.replace('最后统计时间', '').trim();
                }
                if (!info.planName && (l.includes('Lite') || (l.includes('套餐') && l.length < 20))) {
                    info.planName = l;
                }
            }

            console.log('[Aliyun] 提取的数据:', info);
            return (info.status || info.startTime) ? info : null;
        } catch (e) {
            console.error('[Aliyun] 数据提取失败:', e);
            return null;
        }
    }

    function buildProviderData(info) {
        const validDate = info.startTime && info.endTime
            ? `${info.startTime} ~ ${info.endTime}`
            : (info.endTime || '--');

        const items = [
            { key: '套餐',    value: info.planName      || '未知套餐', highlight: false },
            { key: '状态',    value: info.status        || '--',      highlight: true  },
            { key: '剩余天数', value: info.remainingDays || '--',      highlight: false },
            { key: '有效期',  value: validDate,                        highlight: false },
        ];

        if (info.lastStatTime) {
            items.push({ key: '统计时间', value: info.lastStatTime, highlight: false });
        }

        // 将用量数据转换为配额分组格式（仅显示已用量，无限额）
        const quota_groups = [
            { label: '近5小时', used: info.usage5h || '--', limit: 0, remain: '--', highlight: false },
            { label: '近一周',  used: info.usageWeek || '--', limit: 0, remain: '--', highlight: false },
            { label: '近一月',  used: info.usageMonth || '--', limit: 0, remain: '--', highlight: false },
        ];

        const isExpired    = (info.status || '').includes('过期');
        const compact_text = isExpired
            ? '状态：' + info.status
            : '近5H：' + (info.usage5h || '--');

        return { provider_id: PROVIDER_ID, provider_name: PROVIDER_NAME, items, quota_groups, compact_text };
    }

    // ── 页面刷新 ──────────────────────────────────────────────────────────────
    // 查找并点击阿里云刷新按钮

    function triggerPageRefresh() {
        console.log('[Aliyun] 尝试触发页面刷新');
        
        try {
            const sparkIcons = document.querySelectorAll('.spark-icon-spark-refresh-line');
            if (sparkIcons.length === 0) {
                console.log('[Aliyun] 未找到刷新按钮 (.spark-icon-spark-refresh-line)');
                return false;
            }
            
            console.log('[Aliyun] 找到', sparkIcons.length, '个刷新按钮');
            let clickedCount = 0;
            
            sparkIcons.forEach((icon, index) => {
                const clickable = icon.closest('button, span[role="button"], a, [onclick]');
                if (clickable) {
                    console.log('[Aliyun] 点击第', index + 1, '个刷新按钮');
                    clickable.click();
                    clickedCount++;
                } else if (icon.onclick || icon.getAttribute('onclick')) {
                    console.log('[Aliyun] 直接点击第', index + 1, '个刷新图标');
                    icon.click();
                    clickedCount++;
                }
            });
            
            console.log('[Aliyun] 共点击了', clickedCount, '个刷新按钮');
            return clickedCount > 0;
        } catch (e) {
            console.error('[Aliyun] 刷新按钮点击失败:', e);
            return false;
        }
    }

    // ── 数据拉取 ─────────────────────────────────────────────────────────────────
    // 仅在 URL 包含 bailian.console.aliyun.com 时执行 DOM 抓取。
    // 每次拉取数据前先触发页面刷新，然后等待页面更新后提取数据。
    // 成功推送后才更新 lastFetchAt；未取到数据时保持原値 → tick 每 2s 重试直到成功。

    function fetchData() {
        const url = window.location.href;
        console.log('[Aliyun] fetchData 被调用, URL:', url);
        if (!url.includes('bailian.console.aliyun.com')) {
            console.log('[Aliyun] URL 不匹配，跳过数据提取');
            return;
        }
        
        // 每次拉取数据前先触发页面刷新
        triggerPageRefresh();
        
        // 触发刷新后等待 5 秒让页面更新
        setTimeout(() => {
            console.log('[Aliyun] 刷新后延迟提取数据');
            extractAndSendData();
        }, 5000);
    }
    
    function extractAndSendData() {
        const info = extractPlanInfo();
        if (!info) {
            console.log('[Aliyun] 未提取到数据，等待重试');
            return; // 未取到数据：不更新 lastFetchAt → 2s 后重试
        }
        console.log('[Aliyun] 数据提取成功，准备发送');
        emitToRust('provider_data_updated', buildProviderData(info));
        lastFetchAt = Date.now(); // 仅在成功推送后更新
    }

    // ── 主循环 ────────────────────────────────────────────────────────────────
    // 登录判定：login_aliyunid cookie 存在且非空 → 已登录（纯 Cookie，不依赖 URL）。
    //
    // prevLoggedIn 状态机：
    //   null / false → true : emit provider_login_detected  + 立即拉取数据
    //   true         → false: emit provider_logout_detected
    //
    // 已登录期间每 FETCH_INTERVAL ms 拉取一次数据。

    function tick() {
        // 如果检测被暂停，跳过本次检测
        if (checkPaused) {
            console.log('[Aliyun] 检测已暂停，跳过本次 tick');
            return;
        }

        const loginId  = getCookie('login_aliyunid').trim();
        const loggedIn = loginId.length > 0;

        console.log('[Aliyun] tick - loggedIn:', loggedIn, 'prevLoggedIn:', prevLoggedIn);

        if (loggedIn !== prevLoggedIn) {
            if (loggedIn) {
                // null/false → true
                console.log('[Aliyun] 检测到登录');
                prevLoggedIn = true;
                emitToRust('provider_login_detected', { provider_id: PROVIDER_ID });
                // Rust 侧收到后会立即推占位数据显示悬浮窗。
                // lastFetchAt 保持 0 → 下方区间检查（Date.now()-0 >= 60000）立即为 true
                // → 本 tick 末尾或下次 tick 立即调用 fetchData()，失败则持续重试直到成功。
            } else {
                if (prevLoggedIn === true) {
                    // true → false
                    console.log('[Aliyun] 检测到登出');
                    emitToRust('provider_logout_detected', { provider_id: PROVIDER_ID });
                }
                prevLoggedIn = false;
            }
        }

        // 已登录时按固定间隔定时拉取数据
        if (prevLoggedIn === true && Date.now() - lastFetchAt >= FETCH_INTERVAL) {
            console.log('[Aliyun] 触发数据拉取');
            fetchData();
        }
    }

    console.log('[Aliyun] 启动定时器');
    setInterval(tick, 2000);
    window.addEventListener('load', () => {
        console.log('[Aliyun] 页面加载完成，执行 tick');
        tick();
    });
    
    // 立即执行一次
    console.log('[Aliyun] 立即执行首次 tick');
    tick();
})();
    "#.to_string()
}
