//! 中国移动智算包 Provider
//!
//! 与其他 Provider 不同，此 Provider 不需要登录页面，而是通过 API Key 查询套餐余量。
//! 用户在配置页面输入 API Key，后台定时请求 API 并推送数据到悬浮窗。

use super::ProviderConfig;

pub fn provider() -> ProviderConfig {
    ProviderConfig {
        id: "chinamobile",
        name: "中国移动智算包",
        // 直接访问中国移动的套餐页面
        target_url: "https://maas.gd.chinamobile.com:38559/maas/uifm/#/model-quota",
        allowed_domains: vec![
            "https://maas.gd.chinamobile.com:38559".to_string(),
            "https://*.chinamobile.com".to_string(),
        ],
        injection_script: injection_script(),
    }
}

fn injection_script() -> String {
    r#"
(function () {
    console.log('[ChinaMobile] 注入脚本开始执行');
    console.log('[ChinaMobile] 当前 URL:', window.location.href);
    console.log('[ChinaMobile] Tauri API 可用性:', typeof window.__TAURI__);
    
    if (window.__LSYS_CHINAMOBILE_INJECTED__) {
        console.log('[ChinaMobile] 脚本已注入，跳过');
        return;
    }
    window.__LSYS_CHINAMOBILE_INJECTED__ = true;

    const PROVIDER_ID    = 'chinamobile';
    const PROVIDER_NAME  = '中国移动智算包';
    let FETCH_INTERVAL = 60_000; // 默认60秒
    let lastFetchAt = 0;
    let prevLoggedIn = null;
    let currentApiKey = '';
    let isPaused = false; // 暂停标志，窗口可见时暂停定时任务

    // 暴露全局函数供 Rust 侧动态修改刷新间隔
    window.__LSYS_SET_INTERVAL__ = function(intervalMs) {
        FETCH_INTERVAL = intervalMs;
        console.log('[ChinaMobile] 刷新间隔已更新为:', intervalMs, 'ms');
    };

    // ── IPC 工具 ──────────────────────────────────────────────────────────────

    async function emitToRust(eventName, payload) {
        try {
            console.log('[ChinaMobile] 尝试发送事件:', eventName, payload);
            if (window.__TAURI__?.event?.emit) {
                await window.__TAURI__.event.emit(eventName, payload);
                console.log('[ChinaMobile] 事件发送成功:', eventName);
            } else {
                console.error('[ChinaMobile] Tauri API 不可用');
            }
        } catch (e) {
            console.error('[ChinaMobile] emit failed:', eventName, e);
        }
    }

    // ── 页面元素检测 ──────────────────────────────────────────────────────────

    function getApiKeyFromPage() {
        // 从输入框获取 API Key
        const inputs = document.querySelectorAll('.el-input__inner');
        for (const input of inputs) {
            const value = input.value?.trim();
            if (value && value.length > 10) {
                console.log('[ChinaMobile] 找到 API Key 输入框，长度:', value.length);
                return value;
            }
        }
        console.log('[ChinaMobile] 未找到 API Key 输入框');
        return '';
    }

    function checkStatusTag() {
        // 检查状态标签是否显示"正常"
        const statusTags = document.querySelectorAll('.status-tag');
        for (const tag of statusTags) {
            const text = tag.textContent?.trim();
            console.log('[ChinaMobile] 状态标签内容:', text);
            if (text && text.includes('正常')) {
                console.log('[ChinaMobile] 检测到状态"正常"');
                return true;
            }
        }
        console.log('[ChinaMobile] 未检测到状态"正常"');
        return false;
    }

    function isLoggedIn() {
        // 获取 API Key
        const apiKey = getApiKeyFromPage();
        if (!apiKey) {
            console.log('[ChinaMobile] 未找到 API Key');
            return false;
        }

        // 检查状态是否正常
        const isNormal = checkStatusTag();
        if (!isNormal) {
            console.log('[ChinaMobile] 状态不正常');
            return false;
        }

        // 更新当前 API Key
        currentApiKey = apiKey;
        console.log('[ChinaMobile] 登录检测成功，API Key 长度:', apiKey.length);
        return true;
    }

    // ── 数据提取 ──────────────────────────────────────────────────────────────

    async function fetchData() {
        if (!currentApiKey) {
            console.log('[ChinaMobile] API Key 未配置，跳过数据拉取');
            return;
        }

        console.log('[ChinaMobile] 开始拉取数据，API Key 长度:', currentApiKey.length);

        try {
            const url = `https://maas.gd.chinamobile.com:38559/maas/uifm-api/api/portal/packages/usage?apiKey=${currentApiKey}`;
            const response = await fetch(url, {
                method: 'GET',
                headers: {
                    'Accept': 'application/json, text/plain, */*',
                    'Accept-Language': 'zh-CN,zh;q=0.9,en;q=0.8',
                    'Authorization': 'Bearer',
                    'Cache-Control': 'no-store',
                    'Connection': 'keep-alive',
                    'Pragma': 'no-cache',
                    'Referer': 'https://maas.gd.chinamobile.com:38559/maas/uifm/',
                    'Sec-Fetch-Dest': 'empty',
                    'Sec-Fetch-Mode': 'cors',
                    'Sec-Fetch-Site': 'same-origin',
                    'User-Agent': 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/147.0.0.0 Safari/537.36',
                    'X-Request-ID': 'req_' + Date.now(),
                    'X-Timezone': 'Asia/Shanghai',
                    'sec-ch-ua': '"Google Chrome";v="147", "Not.A/Brand";v="8", "Chromium";v="147"',
                    'sec-ch-ua-mobile': '?0',
                    'sec-ch-ua-platform': '"macOS"',
                },
            });

            if (!response.ok) {
                console.error('[ChinaMobile] API 请求失败:', response.status);
                return;
            }

            const data = await response.json();
            console.log('[ChinaMobile] API 响应:', data);

            if (data.code !== 100200 || !data.data) {
                console.error('[ChinaMobile] API 返回错误:', data.msg || '未知错误');
                return;
            }

            // 构建并推送数据
            const providerData = buildProviderData(data.data);
            await emitToRust('provider_data_updated', providerData);
            
            lastFetchAt = Date.now();
            console.log('[ChinaMobile] 数据推送成功');
        } catch (e) {
            console.error('[ChinaMobile] 数据拉取失败:', e);
        }
    }

    function buildProviderData(data) {
        const items = [
            { key: '套餐', value: data.name || '未知套餐', highlight: false },
            { key: '状态', value: data.planStatus === 1 ? '正常' : '异常', highlight: true },
            { key: '到期', value: data.periodEnd || '--', highlight: false },
        ];

        // 构建配额分组
        const quota_groups = [];
        if (data.quotas && Array.isArray(data.quotas)) {
            for (const quota of data.quotas) {
                const used = quota.usageQuota || 0;
                const limit = quota.quotaLimit || 0;
                const remain = limit - used;
                const isLow = remain < limit * 0.2; // 剩余不足20%时高亮

                quota_groups.push({
                    label: quota.windowName || '未知窗口',
                    used: used,
                    limit: limit,
                    remain: remain,
                    highlight: isLow,
                });
            }
        }

        // 紧凑文本：显示第一个配额窗口的剩余量
        let compact_text = PROVIDER_NAME;
        if (quota_groups.length > 0) {
            const first = quota_groups[0];
            compact_text = `${first.label}剩余：${formatNum(first.remain)}`;
        }

        return {
            provider_id: PROVIDER_ID,
            provider_name: PROVIDER_NAME,
            items,
            quota_groups,
            compact_text,
            updated_at: Date.now(),
        };
    }

    function formatNum(n) {
        if (n >= 10000) return (n / 10000).toFixed(1) + 'w';
        return String(n);
    }

    // ── 主循环 ────────────────────────────────────────────────────────────────
    // 登录判定：检查页面输入框的 API Key 和状态标签
    // 已登录期间每 FETCH_INTERVAL ms 拉取一次数据

    async function tick() {
        // 如果暂停，跳过所有检查
        if (isPaused) {
            console.log('[ChinaMobile] 定时任务已暂停');
            return;
        }

        const loggedIn = isLoggedIn();

        console.log('[ChinaMobile] tick - loggedIn:', loggedIn, 'prevLoggedIn:', prevLoggedIn);

        if (loggedIn !== prevLoggedIn) {
            if (loggedIn) {
                // null/false → true：检测到登录
                console.log('[ChinaMobile] 检测到登录');
                prevLoggedIn = true;
                await emitToRust('provider_login_detected', { provider_id: PROVIDER_ID });
                // lastFetchAt 保持 0，下次 tick 立即拉取数据
            } else {
                if (prevLoggedIn === true) {
                    // true → false：检测到登出
                    console.log('[ChinaMobile] 检测到登出');
                    await emitToRust('provider_logout_detected', { provider_id: PROVIDER_ID });
                }
                prevLoggedIn = false;
                currentApiKey = '';
            }
        }

        // 已登录时按固定间隔定时拉取数据
        if (prevLoggedIn === true && Date.now() - lastFetchAt >= FETCH_INTERVAL) {
            console.log('[ChinaMobile] 触发数据拉取');
            await fetchData();
        }
    }

    // ── 监听查询按钮点击 ──────────────────────────────────────────────────────

    function setupButtonListener() {
        console.log('[ChinaMobile] 设置查询按钮监听');
        
        // 使用事件委托监听所有按钮点击
        document.addEventListener('click', async function(event) {
            // 检查点击的元素或其父元素是否是查询按钮
            let target = event.target;
            let clickedButton = null;
            
            // 向上查找最多3层，找到按钮元素
            for (let i = 0; i < 3 && target; i++) {
                if (target.classList && target.classList.contains('el-button--primary')) {
                    clickedButton = target;
                    break;
                }
                target = target.parentElement;
            }
            
            if (clickedButton) {
                console.log('[ChinaMobile] 检测到查询按钮点击');
                
                // 恢复定时任务
                isPaused = false;
                console.log('[ChinaMobile] 定时任务已恢复');
                
                // 等待一小段时间让页面更新
                setTimeout(async () => {
                    console.log('[ChinaMobile] 延迟后执行检查');
                    
                    // 强制重置状态，确保能检测到登录
                    const wasLoggedIn = prevLoggedIn;
                    prevLoggedIn = null;
                    
                    await tick();
                    
                    // 如果检测到登录，立即拉取数据
                    if (prevLoggedIn === true && !wasLoggedIn) {
                        console.log('[ChinaMobile] 首次登录，立即拉取数据');
                        await fetchData();
                    }
                }, 500);
            }
        }, true); // 使用捕获阶段确保能捕获到事件
        
        console.log('[ChinaMobile] 查询按钮监听已设置');
    }

    // ── 窗口可见性监听 ────────────────────────────────────────────────────────
    // 当窗口从隐藏变为可见时（重新登录），暂停定时任务并重置状态

    function setupVisibilityListener() {
        console.log('[ChinaMobile] 设置窗口可见性监听');
        
        document.addEventListener('visibilitychange', function() {
            if (!document.hidden) {
                console.log('[ChinaMobile] 窗口变为可见，暂停定时任务并重置登录状态');
                // 窗口变为可见时，暂停定时任务
                isPaused = true;
                // 重置状态以便重新检测
                prevLoggedIn = null;
                currentApiKey = '';
                lastFetchAt = 0;
            } else {
                console.log('[ChinaMobile] 窗口变为隐藏');
                // 窗口隐藏时，确保定时任务恢复
                isPaused = false;
            }
        });
        
        console.log('[ChinaMobile] 窗口可见性监听已设置');
    }

    console.log('[ChinaMobile] 启动定时器');
    setInterval(tick, 2000);
    
    window.addEventListener('load', () => {
        console.log('[ChinaMobile] 页面加载完成，执行 tick');
        setupButtonListener();
        setupVisibilityListener();
        tick();
    });
    
    // 立即执行一次
    console.log('[ChinaMobile] 立即执行首次初始化');
    setTimeout(() => {
        setupButtonListener();
        setupVisibilityListener();
        tick();
    }, 100);
})();
    "#.to_string()
}
