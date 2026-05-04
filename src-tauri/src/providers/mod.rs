//! Provider 抽象层
//!
//! 每个 Provider 代表一个需要登录监控的外部网站。
//! 新增 Provider 只需在此目录新建文件并实现 [`ProviderConfig`]，
//! 然后在 [`all_providers`] 中注册即可。

use serde::{Deserialize, Serialize};

// ─── 数据结构（供 Overlay 渲染使用）────────────────────────────────────────────

/// 单条展示数据（Key-Value 对）
///
/// 此结构体是 Rust ↔ JS 数据协议的 Schema 文档，
/// 未来如需在 Rust 侧校验或二次处理数据时直接启用反序列化即可。
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataItem {
    /// 标签文字，如 "套餐"、"状态"
    pub key: String,
    /// 显示值
    pub value: String,
    /// 是否以强调色渲染（如状态字段用绿/红色）
    #[serde(default)]
    pub highlight: bool,
}

/// Provider 向 Overlay 推送的标准化数据载荷
///
/// 对应 JS 中 `emitToRust('provider_data_updated', data)` 的 payload 结构。
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderData {
    /// Provider 唯一标识，如 "aliyun"
    pub provider_id: String,
    /// Provider 显示名称，如 "阿里云百炼"
    pub provider_name: String,
    /// 有序的数据项列表，Overlay 按顺序渲染
    pub items: Vec<DataItem>,
    /// 折叠/小块模式下的单行摘要文字，如 "近5H：1234 tokens"
    pub compact_text: String,
}

// ─── Provider 配置 ─────────────────────────────────────────────────────────────

/// 描述一个 Provider 的静态配置。
///
/// 每个 Provider 提供：
/// - 登录入口 URL（WebView 初始加载地址）
/// - 目标页 URL（提取数据的页面）
/// - 注入脚本（登录检测 + 数据提取 + 事件上报）
/// - 允许访问 Tauri IPC 的域名列表
pub struct ProviderConfig {
    /// 唯一标识符，用于生成窗口 label，如 "aliyun"
    pub id: &'static str,
    /// 展示名称（中文），如 "阿里云百炼"
    pub name: &'static str,
    /// WebView 加载的目标页 URL（未登录时页面会自行弹出登录框）
    pub target_url: &'static str,
    /// 允许该 Provider 的 WebView 访问 Tauri IPC 的域名通配列表
    /// （对应 capabilities/*.json 中的 remote.urls，此处作文档性声明）
    #[allow(dead_code)]
    pub allowed_domains: Vec<String>,
    /// 注入到 Provider WebView 每个页面的 JavaScript
    pub injection_script: String,
}

impl ProviderConfig {
    /// 返回该 Provider 对应的 Tauri 窗口 label，格式为 `provider_{id}`
    pub fn window_label(&self) -> String {
        format!("provider_{}", self.id)
    }
}

// ─── Provider 子模块 ────────────────────────────────────────────────────────────

pub mod aliyun;
pub mod baidu;
pub mod volcengine;
pub mod chinamobile;

/// 返回所有已注册的 Provider 配置列表
///
/// 新增 Provider 时，在此处追加 `new_provider::provider()` 即可
pub fn all_providers() -> Vec<ProviderConfig> {
    vec![
        aliyun::provider(),
        baidu::provider(),
        volcengine::provider(),
        chinamobile::provider(),
    ]
}
