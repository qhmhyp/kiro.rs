//! Kiro API Provider
//!
//! 核心组件，负责与 Kiro API 通信
//! 支持流式和非流式请求
//! 支持多凭据故障转移和重试
//! 支持按凭据级 endpoint 切换不同 Kiro API 端点

use reqwest::Client;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::time::sleep;

use crate::http_client::{ProxyConfig, build_client};
use crate::kiro::endpoint::{KiroEndpoint, RequestContext};
use crate::kiro::machine_id;
use crate::kiro::model::credentials::KiroCredentials;
use crate::kiro::token_manager::MultiTokenManager;
use crate::model::config::TlsBackend;
use parking_lot::Mutex;

/// 每个凭据的最大重试次数
const MAX_RETRIES_PER_CREDENTIAL: usize = 3;

/// 总重试次数硬上限（避免无限重试）
const MAX_TOTAL_RETRIES: usize = 9;

/// 上游错误结构化信息（通过 [`anyhow::Error::downcast_ref`] 获取）
///
/// 用于让 HTTP handler 保留上游真实状态码与错误分类，同时把详细响应体留在日志和
/// 内部状态中，避免把上游诊断细节直接返回给客户端。
#[derive(Debug, Clone)]
pub struct UpstreamError {
    /// 上游 HTTP 状态码；网络层 / 链路错误时为 None
    pub status: Option<u16>,
    /// 上游响应体或错误描述（已截断到 ~1KB，按字符截断避免 UTF-8 边界 panic）
    pub body: String,
    /// 触发错误时所用的凭据 ID（便于排查到具体账号）
    pub credential_id: u64,
    /// true 表示所有凭据都已用尽（被禁用 / 配额耗尽 / 冷却中）
    pub all_credentials_exhausted: bool,
}

impl UpstreamError {
    /// 按字符（非字节）截断 body，避免在 UTF-8 多字节字符边界 panic
    fn truncate_body(body: &str, max_chars: usize) -> String {
        if body.chars().count() > max_chars {
            let mut s: String = body.chars().take(max_chars).collect();
            s.push_str("...");
            s
        } else {
            body.to_string()
        }
    }

    /// 构造一个上游错误，body 自动截断到 1KB
    pub fn new(status: Option<u16>, body: &str, credential_id: u64) -> Self {
        Self {
            status,
            body: Self::truncate_body(body, 1024),
            credential_id,
            all_credentials_exhausted: false,
        }
    }

    /// 标记"所有凭据已用尽"——handler 据此给客户端加 Retry-After
    pub fn exhausted(mut self) -> Self {
        self.all_credentials_exhausted = true;
        self
    }

    /// 包装成 anyhow::Error，便于沿用现有的 anyhow::Result 签名
    pub fn into_anyhow(self) -> anyhow::Error {
        anyhow::Error::new(self)
    }
}

impl std::fmt::Display for UpstreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let exhausted = if self.all_credentials_exhausted {
            "（所有凭据已用尽）"
        } else {
            ""
        };
        match self.status {
            Some(s) => write!(
                f,
                "上游返回 {} （凭据 #{}{}）: {}",
                s, self.credential_id, exhausted, self.body
            ),
            None => write!(
                f,
                "上游链路错误 （凭据 #{}{}）: {}",
                self.credential_id, exhausted, self.body
            ),
        }
    }
}

impl std::error::Error for UpstreamError {}

/// 单凭据验证结果（[`KiroProvider::send_once_with_credential`] 的返回值）
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyOutcome {
    /// 上游返回 2xx 时为 true
    pub ok: bool,
    /// 上游 HTTP 状态码；网络层失败时为 None
    pub status: Option<u16>,
    /// 端到端耗时（毫秒）
    pub latency_ms: u64,
    /// 失败时填充错误信息或响应体摘要
    pub error: Option<String>,
}

impl VerifyOutcome {
    fn failure(status: Option<u16>, latency_ms: u64, error: String) -> Self {
        Self {
            ok: false,
            status,
            latency_ms,
            error: Some(error),
        }
    }
}

/// Kiro API Provider
///
/// 核心组件，负责与 Kiro API 通信
/// 支持多凭据故障转移和重试机制
/// 按凭据 `endpoint` 字段选择 [`KiroEndpoint`] 实现
pub struct KiroProvider {
    token_manager: Arc<MultiTokenManager>,
    /// 全局代理配置（用于凭据无自定义代理时的回退）
    global_proxy: Option<ProxyConfig>,
    /// Client 缓存：key = effective proxy config, value = reqwest::Client
    /// 不同代理配置的凭据使用不同的 Client，共享相同代理的凭据复用 Client
    client_cache: Mutex<HashMap<Option<ProxyConfig>, Client>>,
    /// TLS 后端配置
    tls_backend: TlsBackend,
    /// 端点实现注册表（key: endpoint 名称）
    endpoints: HashMap<String, Arc<dyn KiroEndpoint>>,
    /// 默认端点名称（凭据未指定 endpoint 时使用）
    default_endpoint: String,
}

impl KiroProvider {
    /// 创建带代理配置和端点注册表的 KiroProvider 实例
    ///
    /// # Arguments
    /// * `token_manager` - 多凭据 Token 管理器
    /// * `proxy` - 全局代理配置
    /// * `endpoints` - 端点名 → 实现的注册表（至少包含 `default_endpoint` 对应条目）
    /// * `default_endpoint` - 凭据未显式指定 endpoint 时使用的名称
    pub fn with_proxy(
        token_manager: Arc<MultiTokenManager>,
        proxy: Option<ProxyConfig>,
        endpoints: HashMap<String, Arc<dyn KiroEndpoint>>,
        default_endpoint: String,
    ) -> Self {
        assert!(
            endpoints.contains_key(&default_endpoint),
            "默认端点 {} 未在 endpoints 注册表中",
            default_endpoint
        );
        let tls_backend = token_manager.config().tls_backend;
        // 预热：构建全局代理对应的 Client
        let initial_client = build_client(proxy.as_ref(), 720, tls_backend)
            .expect("创建 HTTP 客户端失败");
        let mut cache = HashMap::new();
        cache.insert(proxy.clone(), initial_client);

        Self {
            token_manager,
            global_proxy: proxy,
            client_cache: Mutex::new(cache),
            tls_backend,
            endpoints,
            default_endpoint,
        }
    }

    /// 构造一个上游错误，同时把"最近错误"记录到 token_manager 供 admin UI 展示
    ///
    /// 参数顺序与 [`UpstreamError::new`] 一致，方便从调用点统一替换。
    fn upstream_error_for(
        &self,
        status: Option<u16>,
        body: &str,
        credential_id: u64,
    ) -> UpstreamError {
        self.token_manager.set_last_error(credential_id, status, body);
        UpstreamError::new(status, body, credential_id)
    }

    /// 根据凭据的代理配置获取（或创建并缓存）对应的 reqwest::Client
    fn client_for(&self, credentials: &KiroCredentials) -> anyhow::Result<Client> {
        let effective = credentials.effective_proxy(self.global_proxy.as_ref());
        let mut cache = self.client_cache.lock();
        if let Some(client) = cache.get(&effective) {
            return Ok(client.clone());
        }
        let client = build_client(effective.as_ref(), 720, self.tls_backend)?;
        cache.insert(effective, client.clone());
        Ok(client)
    }

    /// 根据凭据选择 endpoint 实现
    fn endpoint_for(
        &self,
        credentials: &KiroCredentials,
    ) -> anyhow::Result<Arc<dyn KiroEndpoint>> {
        let name = credentials
            .endpoint
            .as_deref()
            .unwrap_or(&self.default_endpoint);
        self.endpoints
            .get(name)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("未知端点: {}", name))
    }

    /// 发送非流式 API 请求
    ///
    /// 支持多凭据故障转移（见 [`Self::call_api_with_retry`]）
    /// 返回 (Response, 凭据id, InFlightGuard)；guard 存活到调用方丢弃，计数自动归还。
    pub async fn call_api(
        &self,
        request_body: &str,
        conversation_id: Option<&str>,
    ) -> anyhow::Result<(reqwest::Response, u64, crate::kiro::in_flight::InFlightGuard)> {
        self.call_api_with_retry(request_body, false, conversation_id)
            .await
    }

    /// 暴露底层 token manager（供 anthropic 层记账金额）
    pub fn token_manager(&self) -> std::sync::Arc<MultiTokenManager> {
        self.token_manager.clone()
    }

    /// 用指定凭据发送一次非流式 API 请求，**不重试、不切换凭据、不冷却**。
    ///
    /// 用于"验证单个凭据"场景：调用方明确指定凭据 id，期望即使该凭据被禁用或在
    /// 冷却中也能拿到 token 发出一次真实请求并返回结构化结果。
    pub async fn send_once_with_credential(
        &self,
        id: u64,
        request_body: &str,
    ) -> VerifyOutcome {
        let start = Instant::now();

        let ctx = match self.token_manager.acquire_context_for_id(id).await {
            Ok(c) => c,
            Err(e) => {
                return VerifyOutcome::failure(
                    None,
                    start.elapsed().as_millis() as u64,
                    format!("获取凭据上下文失败: {}", e),
                );
            }
        };

        let _in_flight_guard = self.token_manager.track_request_start(ctx.id);

        let config = self.token_manager.config();
        let machine_id = machine_id::generate_from_credentials(&ctx.credentials, config);

        let endpoint = match self.endpoint_for(&ctx.credentials) {
            Ok(e) => e,
            Err(e) => {
                return VerifyOutcome::failure(
                    None,
                    start.elapsed().as_millis() as u64,
                    format!("endpoint 解析失败: {}", e),
                );
            }
        };

        let rctx = RequestContext {
            credentials: &ctx.credentials,
            token: &ctx.token,
            machine_id: &machine_id,
            config,
        };

        let url = endpoint.api_url(&rctx);
        let body = endpoint.transform_api_body(request_body, &rctx);

        let client = match self.client_for(&ctx.credentials) {
            Ok(c) => c,
            Err(e) => {
                return VerifyOutcome::failure(
                    None,
                    start.elapsed().as_millis() as u64,
                    format!("HTTP client 构建失败: {}", e),
                );
            }
        };

        let base = client
            .post(&url)
            .body(body)
            .header("content-type", "application/json")
            .header("Connection", "close");
        let request = endpoint.decorate_api(base, &rctx);

        match request.send().await {
            Ok(resp) => {
                let status = resp.status();
                let latency_ms = start.elapsed().as_millis() as u64;
                if status.is_success() {
                    VerifyOutcome {
                        ok: true,
                        status: Some(status.as_u16()),
                        latency_ms,
                        error: None,
                    }
                } else {
                    let body_text = resp.text().await.unwrap_or_default();
                    // 按字符（非字节）截断，避免在 UTF-8 多字节字符（CJK / emoji）
                    // 边界上 panic。`&str[..n]` 是字节索引，越界会 crash 当前请求。
                    let preview = if body_text.chars().count() > 500 {
                        let truncated: String = body_text.chars().take(500).collect();
                        format!("{}...", truncated)
                    } else {
                        body_text
                    };
                    VerifyOutcome {
                        ok: false,
                        status: Some(status.as_u16()),
                        latency_ms,
                        error: Some(preview),
                    }
                }
            }
            Err(e) => VerifyOutcome::failure(
                None,
                start.elapsed().as_millis() as u64,
                format!("请求发送失败: {}", e),
            ),
        }
    }

    /// 发送流式 API 请求
    /// 返回 (Response, 凭据id, InFlightGuard)；guard 存活到调用方丢弃，计数自动归还。
    pub async fn call_api_stream(
        &self,
        request_body: &str,
        conversation_id: Option<&str>,
    ) -> anyhow::Result<(reqwest::Response, u64, crate::kiro::in_flight::InFlightGuard)> {
        self.call_api_with_retry(request_body, true, conversation_id)
            .await
    }

    /// 发送 MCP API 请求（WebSearch 等工具调用）
    pub async fn call_mcp(&self, request_body: &str) -> anyhow::Result<reqwest::Response> {
        self.call_mcp_with_retry(request_body).await
    }

    /// 内部方法：带重试逻辑的 MCP API 调用
    async fn call_mcp_with_retry(&self, request_body: &str) -> anyhow::Result<reqwest::Response> {
        let total_credentials = self.token_manager.total_count();
        let max_retries = (total_credentials * MAX_RETRIES_PER_CREDENTIAL).min(MAX_TOTAL_RETRIES);
        let mut last_error: Option<anyhow::Error> = None;
        let mut force_refreshed: HashSet<u64> = HashSet::new();

        for attempt in 0..max_retries {
            // MCP 调用（WebSearch 等工具）不涉及模型选择和会话粘性
            let ctx = match self.token_manager.acquire_context(None, None).await {
                Ok(c) => c,
                Err(e) => {
                    last_error = Some(e);
                    continue;
                }
            };

            let _in_flight_guard = self.token_manager.track_request_start(ctx.id);

            let config = self.token_manager.config();
            let machine_id = machine_id::generate_from_credentials(&ctx.credentials, config);

            let endpoint = match self.endpoint_for(&ctx.credentials) {
                Ok(e) => e,
                Err(e) => {
                    last_error = Some(e);
                    // endpoint 解析失败：记为失败，换下一张凭据
                    self.token_manager.report_failure(ctx.id);
                    continue;
                }
            };

            let rctx = RequestContext {
                credentials: &ctx.credentials,
                token: &ctx.token,
                machine_id: &machine_id,
                config,
            };

            let url = endpoint.mcp_url(&rctx);
            let body = endpoint.transform_mcp_body(request_body, &rctx);

            let base = self
                .client_for(&ctx.credentials)?
                .post(&url)
                .body(body)
                .header("content-type", "application/json")
                .header("Connection", "close");
            let request = endpoint.decorate_mcp(base, &rctx);

            let response = match request.send().await {
                Ok(resp) => resp,
                Err(e) => {
                    tracing::warn!(
                        "MCP 请求发送失败（尝试 {}/{}）: {}",
                        attempt + 1,
                        max_retries,
                        e
                    );
                    last_error =
                        Some(self.upstream_error_for(None, &e.to_string(), ctx.id).into_anyhow());
                    if attempt + 1 < max_retries {
                        sleep(Self::retry_delay(attempt)).await;
                    }
                    continue;
                }
            };

            let status = response.status();

            // 成功响应
            if status.is_success() {
                self.token_manager.report_success(ctx.id);
                return Ok(response);
            }

            // 失败响应
            let body = response.text().await.unwrap_or_default();

            // 402 额度用尽
            if status.as_u16() == 402 && endpoint.is_monthly_request_limit(&body) {
                let has_available = self.token_manager.report_quota_exhausted(ctx.id);
                if !has_available {
                    return Err(self.upstream_error_for(Some(status.as_u16()), &body, ctx.id)
                        .exhausted()
                        .into_anyhow());
                }
                last_error =
                    Some(self.upstream_error_for(Some(status.as_u16()), &body, ctx.id).into_anyhow());
                continue;
            }

            // 400 Bad Request
            if status.as_u16() == 400 {
                return Err(self.upstream_error_for(Some(400), &body, ctx.id).into_anyhow());
            }

            // 401/403 凭据问题
            if matches!(status.as_u16(), 401 | 403) {
                // token 被上游失效：先尝试 force-refresh，每凭据仅一次机会
                if endpoint.is_bearer_token_invalid(&body) && !force_refreshed.contains(&ctx.id) {
                    force_refreshed.insert(ctx.id);
                    tracing::info!("凭据 #{} token 疑似被上游失效，尝试强制刷新", ctx.id);
                    if self.token_manager.force_refresh_token_for(ctx.id).await.is_ok() {
                        tracing::info!("凭据 #{} token 强制刷新成功，重试请求", ctx.id);
                        continue;
                    }
                    tracing::warn!("凭据 #{} token 强制刷新失败，计入失败", ctx.id);
                }

                let has_available = self.token_manager.report_failure(ctx.id);
                if !has_available {
                    return Err(self.upstream_error_for(Some(status.as_u16()), &body, ctx.id)
                        .exhausted()
                        .into_anyhow());
                }
                last_error =
                    Some(self.upstream_error_for(Some(status.as_u16()), &body, ctx.id).into_anyhow());
                continue;
            }

            // 429 - 上游限流：同主路径逻辑,风控立即冷却,普通 429 连续 5 次才冷却
            if status.as_u16() == 429 {
                let is_suspicious = body.contains("suspicious activity");
                tracing::warn!(
                    "MCP 请求 429（{}，凭据 #{}，尝试 {}/{}）: {}",
                    if is_suspicious { "风控" } else { "瞬态" },
                    ctx.id,
                    attempt + 1,
                    max_retries,
                    body
                );

                if is_suspicious {
                    let cooldown = Duration::from_secs(600);
                    let has_available = self.token_manager.report_rate_limited(ctx.id, cooldown);
                    if !has_available {
                        return Err(self.upstream_error_for(Some(429), &body, ctx.id)
                            .exhausted()
                            .into_anyhow());
                    }
                } else {
                    let should_cooldown = self.token_manager.increment_429_count(ctx.id);
                    if should_cooldown {
                        let cooldown = Duration::from_secs(60);
                        let has_available = self.token_manager.report_rate_limited(ctx.id, cooldown);
                        if !has_available {
                            return Err(self.upstream_error_for(Some(429), &body, ctx.id)
                                .exhausted()
                                .into_anyhow());
                        }
                    } else if attempt + 1 < max_retries {
                        sleep(Self::retry_delay(attempt)).await;
                    }
                }
                last_error = Some(self.upstream_error_for(Some(429), &body, ctx.id).into_anyhow());
                continue;
            }

            // 408/5xx 瞬态错误：在当前凭据上重试
            if status.as_u16() == 408 || status.is_server_error() {
                tracing::warn!(
                    "MCP 请求失败（上游瞬态错误，尝试 {}/{}）: {} {}",
                    attempt + 1,
                    max_retries,
                    status,
                    body
                );
                last_error =
                    Some(self.upstream_error_for(Some(status.as_u16()), &body, ctx.id).into_anyhow());
                if attempt + 1 < max_retries {
                    sleep(Self::retry_delay(attempt)).await;
                }
                continue;
            }

            // 其他 4xx
            if status.is_client_error() {
                return Err(self.upstream_error_for(Some(status.as_u16()), &body, ctx.id).into_anyhow());
            }

            // 兜底
            last_error =
                Some(self.upstream_error_for(Some(status.as_u16()), &body, ctx.id).into_anyhow());
            if attempt + 1 < max_retries {
                sleep(Self::retry_delay(attempt)).await;
            }
        }

        Err(last_error.unwrap_or_else(|| {
            anyhow::anyhow!("MCP 请求失败：已达到最大重试次数（{}次）", max_retries)
        }))
    }

    /// 内部方法：带重试逻辑的 API 调用
    ///
    /// 重试策略：
    /// - 每个凭据最多重试 MAX_RETRIES_PER_CREDENTIAL 次
    /// - 总重试次数 = min(凭据数量 × 每凭据重试次数, MAX_TOTAL_RETRIES)
    /// - 硬上限 9 次，避免无限重试
    async fn call_api_with_retry(
        &self,
        request_body: &str,
        is_stream: bool,
        conversation_id: Option<&str>,
    ) -> anyhow::Result<(reqwest::Response, u64, crate::kiro::in_flight::InFlightGuard)> {
        let total_credentials = self.token_manager.total_count();
        let max_retries = (total_credentials * MAX_RETRIES_PER_CREDENTIAL).min(MAX_TOTAL_RETRIES);
        let mut last_error: Option<anyhow::Error> = None;
        let mut force_refreshed: HashSet<u64> = HashSet::new();
        let api_type = if is_stream { "流式" } else { "非流式" };

        // 尝试从请求体中提取模型信息
        let model = Self::extract_model_from_request(request_body);

        for attempt in 0..max_retries {
            // 获取调用上下文（绑定 index、credentials、token）
            let ctx = match self
                .token_manager
                .acquire_context(model.as_deref(), conversation_id)
                .await
            {
                Ok(c) => c,
                Err(e) => {
                    last_error = Some(e);
                    continue;
                }
            };

            // 每次上游尝试都登记:在途 +1(guard 随本轮作用域 drop 自动归还)、窗口记 start
            let in_flight_guard = self.token_manager.track_request_start(ctx.id);

            let config = self.token_manager.config();
            let machine_id = machine_id::generate_from_credentials(&ctx.credentials, config);

            let endpoint = match self.endpoint_for(&ctx.credentials) {
                Ok(e) => e,
                Err(e) => {
                    last_error = Some(e);
                    self.token_manager.report_failure(ctx.id);
                    continue;
                }
            };

            let rctx = RequestContext {
                credentials: &ctx.credentials,
                token: &ctx.token,
                machine_id: &machine_id,
                config,
            };

            let url = endpoint.api_url(&rctx);
            let body = endpoint.transform_api_body(request_body, &rctx);

            let base = self
                .client_for(&ctx.credentials)?
                .post(&url)
                .body(body)
                .header("content-type", "application/json")
                .header("Connection", "close");
            let request = endpoint.decorate_api(base, &rctx);

            let response = match request.send().await {
                Ok(resp) => resp,
                Err(e) => {
                    tracing::warn!(
                        "API 请求发送失败（尝试 {}/{}）: {}",
                        attempt + 1,
                        max_retries,
                        e
                    );
                    // 网络错误通常是上游/链路瞬态问题，不应导致"禁用凭据"或"切换凭据"
                    // （否则一段时间网络抖动会把所有凭据都误禁用，需要重启才能恢复）
                    last_error =
                        Some(self.upstream_error_for(None, &e.to_string(), ctx.id).into_anyhow());
                    if attempt + 1 < max_retries {
                        sleep(Self::retry_delay(attempt)).await;
                    }
                    continue;
                }
            };

            let status = response.status();

            // 成功响应
            if status.is_success() {
                self.token_manager.report_success(ctx.id);
                return Ok((response, ctx.id, in_flight_guard));
            }

            // 失败响应：读取 body 用于日志/错误信息
            let body = response.text().await.unwrap_or_default();

            // 402 Payment Required 且额度用尽：禁用凭据并故障转移
            if status.as_u16() == 402 && endpoint.is_monthly_request_limit(&body) {
                tracing::warn!(
                    "API 请求失败（额度已用尽，禁用凭据并切换，尝试 {}/{}）: {} {}",
                    attempt + 1,
                    max_retries,
                    status,
                    body
                );

                self.token_manager.log_rate_limit_incident(
                    ctx.id,
                    "402_quota",
                    model.as_deref(),
                    attempt as u32 + 1,
                    None,
                );
                let has_available = self.token_manager.report_quota_exhausted(ctx.id);
                if !has_available {
                    return Err(self.upstream_error_for(Some(status.as_u16()), &body, ctx.id)
                        .exhausted()
                        .into_anyhow());
                }

                last_error =
                    Some(self.upstream_error_for(Some(status.as_u16()), &body, ctx.id).into_anyhow());
                continue;
            }

            // 400 Bad Request - 请求问题，重试/切换凭据无意义
            if status.as_u16() == 400 {
                return Err(self.upstream_error_for(Some(400), &body, ctx.id).into_anyhow());
            }

            // 401/403 - 更可能是凭据/权限问题：计入失败并允许故障转移
            if matches!(status.as_u16(), 401 | 403) {
                tracing::warn!(
                    "API 请求失败（可能为凭据错误，尝试 {}/{}）: {} {}",
                    attempt + 1,
                    max_retries,
                    status,
                    body
                );

                // token 被上游失效：先尝试 force-refresh，每凭据仅一次机会
                if endpoint.is_bearer_token_invalid(&body) && !force_refreshed.contains(&ctx.id) {
                    force_refreshed.insert(ctx.id);
                    tracing::info!("凭据 #{} token 疑似被上游失效，尝试强制刷新", ctx.id);
                    if self.token_manager.force_refresh_token_for(ctx.id).await.is_ok() {
                        tracing::info!("凭据 #{} token 强制刷新成功，重试请求", ctx.id);
                        continue;
                    }
                    tracing::warn!("凭据 #{} token 强制刷新失败，计入失败", ctx.id);
                }

                self.token_manager.log_rate_limit_incident(
                    ctx.id,
                    "40x_auth",
                    model.as_deref(),
                    attempt as u32 + 1,
                    None,
                );
                let has_available = self.token_manager.report_failure(ctx.id);
                if !has_available {
                    return Err(self.upstream_error_for(Some(status.as_u16()), &body, ctx.id)
                        .exhausted()
                        .into_anyhow());
                }

                last_error =
                    Some(self.upstream_error_for(Some(status.as_u16()), &body, ctx.id).into_anyhow());
                continue;
            }

            // 429 - 上游限流：
            // - 风控类("suspicious activity")立即冷却 10 分钟
            // - 普通 429 容忍连续 5 次后才冷却 60 秒,避免偶发限流引起不必要的故障转移级联
            if status.as_u16() == 429 {
                let is_suspicious = body.contains("suspicious activity");
                tracing::warn!(
                    "API 请求 429（{}，凭据 #{}，尝试 {}/{}）: {}",
                    if is_suspicious { "风控" } else { "瞬态" },
                    ctx.id,
                    attempt + 1,
                    max_retries,
                    body
                );

                if is_suspicious {
                    // 风控类：立即冷却 10 分钟
                    let cooldown = Duration::from_secs(600);
                    self.token_manager.log_rate_limit_incident(
                        ctx.id,
                        "429_suspicious",
                        model.as_deref(),
                        attempt as u32 + 1,
                        Some(cooldown.as_secs()),
                    );
                    let has_available = self.token_manager.report_rate_limited(ctx.id, cooldown);
                    if !has_available {
                        return Err(self.upstream_error_for(Some(429), &body, ctx.id)
                            .exhausted()
                            .into_anyhow());
                    }
                } else {
                    // 普通 429：在同一凭据上重试,累计 5 次才冷却。
                    // 不切换凭据——保留 prompt cache 命中,避免切换引发级联。
                    let should_cooldown = self.token_manager.increment_429_count(ctx.id);
                    if should_cooldown {
                        let cooldown = Duration::from_secs(60);
                        self.token_manager.log_rate_limit_incident(
                            ctx.id,
                            "429_transient",
                            model.as_deref(),
                            attempt as u32 + 1,
                            Some(cooldown.as_secs()),
                        );
                        let has_available = self.token_manager.report_rate_limited(ctx.id, cooldown);
                        if !has_available {
                            return Err(self.upstream_error_for(Some(429), &body, ctx.id)
                                .exhausted()
                                .into_anyhow());
                        }
                    } else {
                        // 未达冷却阈值,短暂等待后在同一凭据上重试
                        if attempt + 1 < max_retries {
                            sleep(Self::retry_delay(attempt)).await;
                        }
                    }
                }
                last_error = Some(self.upstream_error_for(Some(429), &body, ctx.id).into_anyhow());
                continue;
            }

            // 408/5xx - 瞬态上游错误：在当前凭据上重试（避免短暂网络/服务抖动把所有凭据锁死）
            if status.as_u16() == 408 || status.is_server_error() {
                tracing::warn!(
                    "API 请求失败（上游瞬态错误，尝试 {}/{}）: {} {}",
                    attempt + 1,
                    max_retries,
                    status,
                    body
                );
                last_error =
                    Some(self.upstream_error_for(Some(status.as_u16()), &body, ctx.id).into_anyhow());
                if attempt + 1 < max_retries {
                    sleep(Self::retry_delay(attempt)).await;
                }
                continue;
            }

            // 其他 4xx - 通常为请求/配置问题：直接返回，不计入凭据失败
            if status.is_client_error() {
                return Err(self.upstream_error_for(Some(status.as_u16()), &body, ctx.id).into_anyhow());
            }

            // 兜底：当作可重试的瞬态错误处理（不切换凭据）
            tracing::warn!(
                "API 请求失败（未知错误，尝试 {}/{}）: {} {}",
                attempt + 1,
                max_retries,
                status,
                body
            );
            last_error =
                Some(self.upstream_error_for(Some(status.as_u16()), &body, ctx.id).into_anyhow());
            if attempt + 1 < max_retries {
                sleep(Self::retry_delay(attempt)).await;
            }
        }

        // 所有重试都失败
        Err(last_error.unwrap_or_else(|| {
            anyhow::anyhow!(
                "{} API 请求失败：已达到最大重试次数（{}次）",
                api_type,
                max_retries
            )
        }))
    }

    /// 从请求体中提取模型信息
    ///
    /// 尝试解析 JSON 请求体，提取 conversationState.currentMessage.userInputMessage.modelId
    fn extract_model_from_request(request_body: &str) -> Option<String> {
        use serde_json::Value;

        let json: Value = serde_json::from_str(request_body).ok()?;

        json.get("conversationState")?
            .get("currentMessage")?
            .get("userInputMessage")?
            .get("modelId")?
            .as_str()
            .map(|s| s.to_string())
    }

    fn retry_delay(attempt: usize) -> Duration {
        // 指数退避 + 少量抖动，避免上游抖动时放大故障
        const BASE_MS: u64 = 200;
        const MAX_MS: u64 = 2_000;
        let exp = BASE_MS.saturating_mul(2u64.saturating_pow(attempt.min(6) as u32));
        let backoff = exp.min(MAX_MS);
        let jitter_max = (backoff / 4).max(1);
        let jitter = fastrand::u64(0..=jitter_max);
        Duration::from_millis(backoff.saturating_add(jitter))
    }
}
