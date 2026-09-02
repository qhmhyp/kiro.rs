//! Admin API 类型定义

use serde::{Deserialize, Serialize};

// ============ 凭据状态 ============

/// 所有凭据状态响应
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialsStatusResponse {
    /// 凭据总数
    pub total: usize,
    /// 可用凭据数量（未禁用）
    pub available: usize,
    /// 全局在途请求总数
    pub total_in_flight: u32,
    /// 当前活跃凭据 ID
    pub current_id: u64,
    /// 各凭据状态列表
    pub credentials: Vec<CredentialStatusItem>,
}

/// 单个凭据的状态信息
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialStatusItem {
    /// 凭据唯一 ID
    pub id: u64,
    /// 优先级（数字越小优先级越高）
    pub priority: u32,
    /// 是否被禁用
    pub disabled: bool,
    /// 连续失败次数
    pub failure_count: u32,
    /// 是否为当前活跃凭据
    pub is_current: bool,
    /// Token 过期时间（RFC3339 格式）
    pub expires_at: Option<String>,
    /// 认证方式
    pub auth_method: Option<String>,
    /// 是否有 Profile ARN
    pub has_profile_arn: bool,
    /// refreshToken 的 SHA-256 哈希（仅 OAuth 凭据，用于前端去重）
    pub refresh_token_hash: Option<String>,
    /// kiroApiKey 的 SHA-256 哈希（仅 API Key 凭据，用于前端去重）
    pub api_key_hash: Option<String>,
    /// kiroApiKey 的脱敏展示（仅 API Key 凭据，用于前端显示）
    pub masked_api_key: Option<String>,
    /// 用户邮箱（用于前端显示）
    pub email: Option<String>,
    /// API 调用成功次数
    pub success_count: u64,
    /// 当前在途请求数(实时并发)
    pub in_flight: u32,
    /// 进程启动以来最高瞬时并发
    pub in_flight_peak: u32,
    /// 近 60s 上游请求数（达到每分钟上限时该凭据暂不参与调度）
    pub req_1m: u32,
    /// 最后一次 API 调用时间（RFC3339 格式）
    pub last_used_at: Option<String>,
    /// 是否配置了凭据级代理
    pub has_proxy: bool,
    /// 代理 URL（用于前端展示）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy_url: Option<String>,
    /// Token 刷新连续失败次数
    pub refresh_failure_count: u32,
    /// 禁用原因
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disabled_reason: Option<String>,
    /// 端点名称（决定该凭据走哪套 Kiro API，已回退到默认端点）
    pub endpoint: String,
    /// 用户自定义名称（前端优先于 email 显示）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// 限流冷却到期时间（RFC3339），到期前该凭据不参与调度
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cooldown_until: Option<String>,
    /// 最近一次上游错误快照（成功调用后清空）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<crate::kiro::token_manager::RecentError>,
    /// 累计消耗金额（USD）
    pub cost_usd: f64,
    /// 累计输入 token
    pub input_tokens_total: u64,
    /// 累计 cache_read token
    pub cache_read_tokens_total: u64,
    /// 累计 cache_creation token
    pub cache_creation_tokens_total: u64,
    /// 累计输出 token
    pub output_tokens_total: u64,
}

// ============ 操作请求 ============

/// 启用/禁用凭据请求
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetDisabledRequest {
    /// 是否禁用
    pub disabled: bool,
}

/// 验证凭据请求（用一次最小 messages 调用测试凭据 + 模型有效性）
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyMessageRequest {
    /// 测试用模型 ID，如 "claude-haiku-4-5"
    pub model: String,
}

/// 部分更新凭据请求（PATCH /credentials/:id）
///
/// 所有字段都是 Optional：`None` 表示该字段不修改；`Some("")` 表示把字段清空（重置为 None）。
/// authMethod 不可改——切换鉴权方式实际上等于换一个凭据，应该删除后重新添加。
/// id / accessToken / expiresAt / subscriptionTitle 由系统维护，也不可改。
/// disabled 有专用端点（POST /:id/disabled），不在此处覆盖。
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCredentialRequest {
    pub name: Option<String>,
    pub refresh_token: Option<String>,
    pub kiro_api_key: Option<String>,
    pub profile_arn: Option<String>,
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
    pub region: Option<String>,
    pub auth_region: Option<String>,
    pub api_region: Option<String>,
    pub machine_id: Option<String>,
    pub email: Option<String>,
    pub proxy_url: Option<String>,
    pub proxy_username: Option<String>,
    pub proxy_password: Option<String>,
    pub endpoint: Option<String>,
    pub priority: Option<u32>,
    /// External IdP token endpoint（authMethod=external_idp 必填）
    pub token_endpoint: Option<String>,
    /// External IdP issuer URL（可选，仅记录用）
    pub issuer_url: Option<String>,
    /// External IdP scopes（空格分隔，含 offline_access 才能拿到 rotating refresh_token）
    pub scopes: Option<String>,
}

/// 修改优先级请求
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetPriorityRequest {
    /// 新优先级值
    pub priority: u32,
}

/// 添加凭据请求
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddCredentialRequest {
    /// 刷新令牌（OAuth 凭据必填，API Key 凭据不需要）
    pub refresh_token: Option<String>,

    /// 认证方式（可选，默认 social）
    #[serde(default = "default_auth_method")]
    pub auth_method: String,

    /// OIDC Client ID（IdC 认证需要）
    pub client_id: Option<String>,

    /// OIDC Client Secret（IdC 认证需要）
    pub client_secret: Option<String>,

    /// 优先级（可选，默认 0）
    #[serde(default)]
    pub priority: u32,

    /// 凭据级 Region 配置（用于 OIDC token 刷新）
    /// 未配置时回退到 config.json 的全局 region
    pub region: Option<String>,

    /// 凭据级 Auth Region（用于 Token 刷新）
    pub auth_region: Option<String>,

    /// 凭据级 API Region（用于 API 请求）
    pub api_region: Option<String>,

    /// 凭据级 Machine ID（可选，64 位字符串）
    /// 未配置时回退到 config.json 的 machineId
    pub machine_id: Option<String>,

    /// 用户邮箱（可选，用于前端显示）
    pub email: Option<String>,

    /// 凭据级代理 URL（可选，特殊值 "direct" 表示不使用代理）
    pub proxy_url: Option<String>,

    /// 凭据级代理认证用户名（可选）
    pub proxy_username: Option<String>,

    /// 凭据级代理认证密码（可选）
    pub proxy_password: Option<String>,

    /// Kiro API Key（API Key 凭据必填，格式: ksk_xxxxxxxx）
    /// 设置后直接作为 Bearer Token 使用，无需 refreshToken
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kiro_api_key: Option<String>,

    /// External IdP token endpoint（authMethod=external_idp 必填）
    ///
    /// 标准 OAuth2 refresh_token grant 的 token endpoint URL
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_endpoint: Option<String>,

    /// External IdP issuer URL（可选，仅记录/审计用）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issuer_url: Option<String>,

    /// External IdP scopes（空格分隔；含 offline_access 才能拿到 rotating refresh_token）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scopes: Option<String>,

    /// 导入短路：若提供且 expiresAt 未到期，则跳过初始刷新，避免烧掉 rotating refresh_token
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access_token: Option<String>,

    /// 导入短路：RFC3339 字符串或 Unix 毫秒(数字),与 accessToken 配合使用
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<ExpiresAt>,

    /// 端点名称（可选，未配置时使用 config.defaultEndpoint）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
}

/// 兼容字符串(RFC3339)和数字(Unix 毫秒)两种 expiresAt 表示
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum ExpiresAt {
    Rfc3339(String),
    UnixMillis(i64),
}

impl ExpiresAt {
    /// 归一化为 RFC3339 字符串；非法值返回 None
    pub fn to_rfc3339(&self) -> Option<String> {
        match self {
            ExpiresAt::Rfc3339(s) => {
                chrono::DateTime::parse_from_rfc3339(s)
                    .ok()
                    .map(|dt| dt.with_timezone(&chrono::Utc).to_rfc3339())
            }
            ExpiresAt::UnixMillis(ms) => chrono::DateTime::<chrono::Utc>::from_timestamp_millis(*ms)
                .map(|dt| dt.to_rfc3339()),
        }
    }
}

fn default_auth_method() -> String {
    "social".to_string()
}

/// 添加凭据成功响应
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddCredentialResponse {
    pub success: bool,
    pub message: String,
    /// 新添加的凭据 ID
    pub credential_id: u64,
    /// 用户邮箱（如果获取成功）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
}

// ============ 余额查询 ============

/// 余额查询响应
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BalanceResponse {
    /// 凭据 ID
    pub id: u64,
    /// 订阅类型
    pub subscription_title: Option<String>,
    /// 当前使用量
    pub current_usage: f64,
    /// 使用限额
    pub usage_limit: f64,
    /// 剩余额度
    pub remaining: f64,
    /// 使用百分比
    pub usage_percentage: f64,
    /// 下次重置时间（Unix 时间戳）
    pub next_reset_at: Option<f64>,
}

// ============ 通用响应 ============

/// 操作成功响应
#[derive(Debug, Serialize)]
pub struct SuccessResponse {
    pub success: bool,
    pub message: String,
}

impl SuccessResponse {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            success: true,
            message: message.into(),
        }
    }
}

/// 错误响应
#[derive(Debug, Serialize)]
pub struct AdminErrorResponse {
    pub error: AdminError,
}

#[derive(Debug, Serialize)]
pub struct AdminError {
    #[serde(rename = "type")]
    pub error_type: String,
    pub message: String,
}

impl AdminErrorResponse {
    pub fn new(error_type: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            error: AdminError {
                error_type: error_type.into(),
                message: message.into(),
            },
        }
    }

    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::new("invalid_request", message)
    }

    pub fn authentication_error() -> Self {
        Self::new("authentication_error", "Invalid or missing admin API key")
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new("not_found", message)
    }

    pub fn api_error(message: impl Into<String>) -> Self {
        Self::new("api_error", message)
    }

    pub fn internal_error(message: impl Into<String>) -> Self {
        Self::new("internal_error", message)
    }
}

// ============ usage 上报设置 ============

/// PATCH /settings/usage-cache 请求（字段均可选，缺省字段保持现值）
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateUsageCacheSettingsRequest {
    /// 是否启用 usage 缓存模拟
    pub enabled: Option<bool>,
    /// 会话空闲过期秒数（0 = 永不过期）
    pub idle_secs: Option<u64>,
    /// cache_read 折扣比例（0.0 ~ 1.0）
    pub read_ratio: Option<f64>,
}

/// usage 上报设置响应（GET 与 PATCH 共用）
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageCacheSettingsResponse {
    pub enabled: bool,
    pub idle_secs: u64,
    pub read_ratio: f64,
    /// 写回 config.json 失败时的警告（设置仍已在运行时生效）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub persist_warning: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_expires_at_rfc3339() {
        let e = ExpiresAt::Rfc3339("2026-06-23T04:24:06Z".to_string());
        let normalized = e.to_rfc3339().unwrap();
        assert!(normalized.starts_with("2026-06-23T04:24:06"));
    }

    #[test]
    fn test_expires_at_rfc3339_with_offset_normalizes_to_utc() {
        let e = ExpiresAt::Rfc3339("2026-06-23T06:24:06+02:00".to_string());
        let normalized = e.to_rfc3339().unwrap();
        assert!(normalized.starts_with("2026-06-23T04:24:06"));
    }

    #[test]
    fn test_expires_at_unix_millis() {
        let e = ExpiresAt::UnixMillis(1_782_189_121_000);
        let normalized = e.to_rfc3339().unwrap();
        assert!(normalized.starts_with("2026-06-23T04:32:01"));
    }

    #[test]
    fn test_expires_at_invalid_rfc3339_returns_none() {
        let e = ExpiresAt::Rfc3339("not a date".to_string());
        assert!(e.to_rfc3339().is_none());
    }

    #[test]
    fn test_add_credential_request_parses_both_expires_at_forms() {
        let json_string = r#"{"refreshToken":"r","authMethod":"external_idp","expiresAt":"2026-06-23T04:24:06Z"}"#;
        let req: AddCredentialRequest = serde_json::from_str(json_string).unwrap();
        assert!(matches!(req.expires_at, Some(ExpiresAt::Rfc3339(_))));

        let json_num = r#"{"refreshToken":"r","authMethod":"external_idp","expiresAt":1782189121000}"#;
        let req: AddCredentialRequest = serde_json::from_str(json_num).unwrap();
        assert!(matches!(req.expires_at, Some(ExpiresAt::UnixMillis(_))));
    }
}
