// 凭据状态响应
export interface CredentialsStatusResponse {
  total: number
  available: number
  currentId: number
  totalInFlight: number
  credentials: CredentialStatusItem[]
}

// 单个凭据状态
export interface CredentialStatusItem {
  id: number
  priority: number
  disabled: boolean
  failureCount: number
  isCurrent: boolean
  expiresAt: string | null
  authMethod: string | null
  hasProfileArn: boolean
  email?: string
  refreshTokenHash?: string
  apiKeyHash?: string
  maskedApiKey?: string
  successCount: number
  inFlight: number
  inFlightPeak: number
  lastUsedAt: string | null
  hasProxy: boolean
  proxyUrl?: string
  refreshFailureCount: number
  disabledReason?: string
  endpoint: string
  name?: string
  cooldownUntil?: string
  lastError?: RecentError
  costUsd: number
  inputTokensTotal: number
  cacheReadTokensTotal: number
  cacheCreationTokensTotal: number
  outputTokensTotal: number
}

// 余额响应
export interface BalanceResponse {
  id: number
  subscriptionTitle: string | null
  currentUsage: number
  usageLimit: number
  remaining: number
  usagePercentage: number
  nextResetAt: number | null
}

// 成功响应
export interface SuccessResponse {
  success: boolean
  message: string
}

// 错误响应
export interface AdminErrorResponse {
  error: {
    type: string
    message: string
  }
}

// 请求类型
export interface SetDisabledRequest {
  disabled: boolean
}

export interface SetPriorityRequest {
  priority: number
}

// 添加凭据请求
export interface AddCredentialRequest {
  refreshToken?: string
  authMethod?: 'social' | 'idc' | 'api_key' | 'external_idp'
  clientId?: string
  clientSecret?: string
  priority?: number
  authRegion?: string
  apiRegion?: string
  region?: string
  machineId?: string
  proxyUrl?: string
  proxyUsername?: string
  proxyPassword?: string
  kiroApiKey?: string
  endpoint?: string
  // External IdP 专用
  tokenEndpoint?: string
  issuerUrl?: string
  scopes?: string
  profileArn?: string
  email?: string
  // 导入短路:携带未过期 access_token 跳过初始刷新,避免烧 rotating refresh_token
  accessToken?: string
  expiresAt?: string | number
}

// 最近一次上游错误（成功调用会清空）
export interface RecentError {
  at: string
  status?: number | null
  bodyPreview: string
}

// 添加凭据响应
export interface AddCredentialResponse {
  success: boolean
  message: string
  credentialId: number
  email?: string
}

// 凭据消息验证响应（POST /credentials/:id/verify-message）
export interface VerifyMessageResponse {
  ok: boolean
  status?: number | null
  latencyMs: number
  error?: string | null
}

// usage 上报设置（GET / PATCH /settings/usage-cache）
export interface UsageCacheSettings {
  enabled: boolean
  idleSecs: number
  readRatio: number
  // 写回 config.json 失败时的警告（设置仍已在运行时生效）
  persistWarning?: string
}

// usage 上报设置更新请求（缺省字段保持现值）
export interface UpdateUsageCacheSettingsRequest {
  enabled?: boolean
  idleSecs?: number
  readRatio?: number
}

// 凭据部分更新请求（PATCH /credentials/:id）
// 字段语义：undefined = 不修改；"" = 清空；其他 = 设为新值
export interface UpdateCredentialRequest {
  name?: string
  refreshToken?: string
  kiroApiKey?: string
  profileArn?: string
  clientId?: string
  clientSecret?: string
  region?: string
  authRegion?: string
  apiRegion?: string
  machineId?: string
  email?: string
  proxyUrl?: string
  proxyUsername?: string
  proxyPassword?: string
  endpoint?: string
  priority?: number
  // External IdP 专用
  tokenEndpoint?: string
  issuerUrl?: string
  scopes?: string
}
