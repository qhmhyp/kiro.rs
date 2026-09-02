use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use crate::pricing::ModelPriceConfig;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum TlsBackend {
    Rustls,
    NativeTls,
}

impl Default for TlsBackend {
    fn default() -> Self {
        Self::Rustls
    }
}

/// KNA 应用配置
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    #[serde(default = "default_host")]
    pub host: String,

    #[serde(default = "default_port")]
    pub port: u16,

    #[serde(default = "default_region")]
    pub region: String,

    /// Auth Region（用于 Token 刷新），未配置时回退到 region
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_region: Option<String>,

    /// API Region（用于 API 请求），未配置时回退到 region
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_region: Option<String>,

    #[serde(default = "default_kiro_version")]
    pub kiro_version: String,

    #[serde(default)]
    pub machine_id: Option<String>,

    #[serde(default)]
    pub api_key: Option<String>,

    #[serde(default = "default_system_version")]
    pub system_version: String,

    #[serde(default = "default_node_version")]
    pub node_version: String,

    #[serde(default = "default_tls_backend")]
    pub tls_backend: TlsBackend,

    /// 外部 count_tokens API 地址（可选）
    #[serde(default)]
    pub count_tokens_api_url: Option<String>,

    /// count_tokens API 密钥（可选）
    #[serde(default)]
    pub count_tokens_api_key: Option<String>,

    /// count_tokens API 认证类型（可选，"x-api-key" 或 "bearer"，默认 "x-api-key"）
    #[serde(default = "default_count_tokens_auth_type")]
    pub count_tokens_auth_type: String,

    /// HTTP 代理地址（可选）
    /// 支持格式: http://host:port, https://host:port, socks5://host:port
    #[serde(default)]
    pub proxy_url: Option<String>,

    /// 代理认证用户名（可选）
    #[serde(default)]
    pub proxy_username: Option<String>,

    /// 代理认证密码（可选）
    #[serde(default)]
    pub proxy_password: Option<String>,

    /// Admin API 密钥（可选，启用 Admin API 功能）
    #[serde(default)]
    pub admin_api_key: Option<String>,

    /// 负载均衡模式（"priority" 或 "balanced"）
    #[serde(default = "default_load_balancing_mode")]
    pub load_balancing_mode: String,

    /// 是否开启非流式响应的 thinking 块提取（默认 true）
    ///
    /// 启用后，非流式响应中的 `<thinking>...</thinking>` 标签会被解析为
    /// 独立的 `{"type": "thinking", ...}` 内容块,与流式响应行为一致。
    #[serde(default = "default_extract_thinking")]
    pub extract_thinking: bool,

    /// 默认端点名称（凭据未显式指定 endpoint 时使用，默认 "ide"）
    #[serde(default = "default_endpoint")]
    pub default_endpoint: String,

    /// 普通 429 触发的冷却时长(秒)。默认 8 秒。
    ///
    /// 实测数据:Kiro 上游真实 throttle 是凭据级短期窗口,触发后 ~5s 同模型恢复 200,
    /// 跨模型 1.3s 内同步 throttle 之后 ~4-7s 恢复,完整恢复 ~10-15s。所以默认 8s
    /// 覆盖约 70-80% 的真实窗口,与"连续 5 次累计才升级"配合,客户端可感的不可用
    /// 窗口压到 ~8s 级。改短(<5s)风险:立刻又被 throttle;改长(>30s)收益甚低。
    /// 风控类 429(suspicious activity)不受此影响,固定 10 分钟。
    #[serde(default = "default_rate_limit_cooldown_secs")]
    pub rate_limit_cooldown_secs: u64,

    /// 是否在返回的 usage 中模拟 prompt cache 命中（默认 true）
    ///
    /// 该模拟不影响真实上游调用，只决定返回给客户端的 usage 拆分：
    /// - true：历史部分拆为 cache_read（0.1× 计费）/ cache_creation（1.25× 计费）
    /// - false：不拆分，全部按 input_tokens 全价上报。下游计费不再享受缓存折扣，
    ///   长对话下计费金额显著提高
    #[serde(default = "default_usage_cache_enabled")]
    pub usage_cache_enabled: bool,

    /// usage 模拟缓存的会话空闲过期时间（秒），默认 300（对齐 Anthropic ephemeral cache）
    ///
    /// 会话空闲超过该时长后，下一轮回到"重建缓存"形态（历史按 cache_creation 1.25× 计费）。
    /// 调小 → 缓存更快失效、计费更高；0 = 永不过期（活跃会话持续享受 cache_read 折扣）。
    /// 仅在 usageCacheEnabled=true 时生效。
    #[serde(default = "default_usage_cache_idle_secs")]
    pub usage_cache_idle_secs: u64,

    /// cache_read 折扣比例（0.0 ~ 1.0），默认 1.0
    ///
    /// 命中缓存的历史部分中，按该比例上报为 cache_read（0.1× 计费），
    /// 其余滑回 input_tokens（1× 全价）。这是"全折扣"与"无折扣"之间的连续旋钮：
    /// 1.0 = 现状（全额折扣）；0.5 = 折扣减半；0.0 = 命中部分全部全价。
    /// 三段之和恒等于真实总输入，下游账单仍然自洽。超出范围自动夹取。
    /// 仅在 usageCacheEnabled=true 时生效。
    #[serde(default = "default_usage_cache_read_ratio")]
    pub usage_cache_read_ratio: f64,

    /// 端点特定的配置
    ///
    /// 键为端点名（如 "ide" / "cli"），值为该端点自由定义的参数对象。
    /// 未在此表出现的端点沿用实现内置默认值。
    #[serde(default)]
    pub endpoints: HashMap<String, serde_json::Value>,

    /// 可选的模型单价覆盖表（key=model id，覆盖/新增内置价格表条目）
    /// 缺省时使用内置默认价。
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pricing: Option<HashMap<String, ModelPriceConfig>>,

    /// 配置文件路径（运行时元数据，不写入 JSON）
    #[serde(skip)]
    config_path: Option<PathBuf>,
}

fn default_host() -> String {
    "127.0.0.1".to_string()
}

fn default_port() -> u16 {
    8080
}

fn default_region() -> String {
    "us-east-1".to_string()
}

fn default_kiro_version() -> String {
    "0.11.107".to_string()
}

fn default_system_version() -> String {
    const SYSTEM_VERSIONS: &[&str] = &["darwin#24.6.0", "win32#10.0.22631"];
    SYSTEM_VERSIONS[fastrand::usize(..SYSTEM_VERSIONS.len())].to_string()
}

fn default_node_version() -> String {
    "22.22.0".to_string()
}

fn default_count_tokens_auth_type() -> String {
    "x-api-key".to_string()
}

fn default_tls_backend() -> TlsBackend {
    TlsBackend::Rustls
}

fn default_load_balancing_mode() -> String {
    "priority".to_string()
}

fn default_extract_thinking() -> bool {
    true
}

fn default_rate_limit_cooldown_secs() -> u64 {
    8
}

fn default_usage_cache_enabled() -> bool {
    true
}

fn default_usage_cache_idle_secs() -> u64 {
    300
}

fn default_usage_cache_read_ratio() -> f64 {
    1.0
}

fn default_endpoint() -> String {
    crate::kiro::endpoint::ide::IDE_ENDPOINT_NAME.to_string()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
            region: default_region(),
            auth_region: None,
            api_region: None,
            kiro_version: default_kiro_version(),
            machine_id: None,
            api_key: None,
            system_version: default_system_version(),
            node_version: default_node_version(),
            tls_backend: default_tls_backend(),
            count_tokens_api_url: None,
            count_tokens_api_key: None,
            count_tokens_auth_type: default_count_tokens_auth_type(),
            proxy_url: None,
            proxy_username: None,
            proxy_password: None,
            admin_api_key: None,
            load_balancing_mode: default_load_balancing_mode(),
            extract_thinking: default_extract_thinking(),
            default_endpoint: default_endpoint(),
            rate_limit_cooldown_secs: default_rate_limit_cooldown_secs(),
            usage_cache_enabled: default_usage_cache_enabled(),
            usage_cache_idle_secs: default_usage_cache_idle_secs(),
            usage_cache_read_ratio: default_usage_cache_read_ratio(),
            endpoints: HashMap::new(),
            pricing: None,
            config_path: None,
        }
    }
}

impl Config {
    /// 获取默认配置文件路径
    pub fn default_config_path() -> &'static str {
        "config.json"
    }

    /// 获取有效的 Auth Region（用于 Token 刷新）
    /// 优先使用 auth_region，未配置时回退到 region
    pub fn effective_auth_region(&self) -> &str {
        self.auth_region.as_deref().unwrap_or(&self.region)
    }

    /// 获取有效的 API Region（用于 API 请求）
    /// 优先使用 api_region，未配置时回退到 region
    pub fn effective_api_region(&self) -> &str {
        self.api_region.as_deref().unwrap_or(&self.region)
    }

    /// 从文件加载配置
    pub fn load<P: AsRef<Path>>(path: P) -> anyhow::Result<Self> {
        let path = path.as_ref();
        if !path.exists() {
            // 配置文件不存在，返回默认配置
            let mut config = Self::default();
            config.config_path = Some(path.to_path_buf());
            return Ok(config);
        }

        let content = fs::read_to_string(path)?;
        let mut config: Config = serde_json::from_str(&content)?;
        config.config_path = Some(path.to_path_buf());
        Ok(config)
    }

}
