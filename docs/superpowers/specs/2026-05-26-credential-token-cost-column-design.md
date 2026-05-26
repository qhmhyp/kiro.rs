# 凭据 Token 消耗金额列

**Status:** Approved · **Date:** 2026-05-26

## 背景与动机

管理员页面的凭据表已有「计数 / 用量」列，但其中的"用量"展示的是 Kiro 订阅**额度**消耗（`currentUsage/usageLimit`），不是金额。运营方需要按**真实 token 消耗折算的美元金额**衡量每个凭据的成本，以便分摊、对账、发现异常凭据。

本设计为每个凭据新增一个**永久累计的消耗金额（USD）**统计，并在管理员页面以新列展示。

## 计费口径

真实 token × 模型单价；cache token 按 Anthropic 倍率精算；金额**永久累计**（语义与现有 `success_count` 一致，不可重置）。

每次**成功**请求按下式累加金额到该次实际使用的凭据：

```text
cost_usd = input_tokens          × input_cost_per_token
         + cache_read_tokens     × cache_read_input_token_cost      (≈ 0.1× input)
         + cache_creation_tokens × cache_creation_input_token_cost  (≈ 1.25× input)
         + output_tokens         × output_cost_per_token
```

- 四段 token 数取自当前请求**最终算定的 usage**（与返回给客户端的 `usage` 字段同源，含 prefix_cache 模拟拆分后的三段输入）。
- 金额在**记录时刻**用当时生效的单价算定后累加。后续改价不回溯重算历史——这符合"按当时价格计费"的正确账务语义。
- 失败/重试中途的请求不计费；只有走到 `report_success` 的那张凭据计费。

## 价格表（`src/pricing.rs`，参考 sub2api 的 LiteLLM 式 schema）

新建模块 `src/pricing.rs`，提供：

```rust
/// 单个模型的单价（USD / token）
pub struct ModelPrice {
    pub input_cost_per_token: f64,
    pub output_cost_per_token: f64,
    pub cache_read_input_token_cost: f64,
    pub cache_creation_input_token_cost: f64,
}

/// 价格表：内置默认 + config 覆盖
pub struct PricingTable { /* HashMap<String, ModelPrice> + fallback */ }

impl PricingTable {
    /// 内置默认表
    pub fn builtin() -> Self;
    /// 用 config.pricing 覆盖/新增条目后返回
    pub fn with_overrides(self, overrides: &HashMap<String, ModelPrice>) -> Self;
    /// 按 model 归一化后查价，未命中返回 fallback
    pub fn price_for(&self, model: &str) -> &ModelPrice;
    /// 计费纯函数
    pub fn cost_usd(&self, model: &str, input: i64, cache_read: i64, cache_creation: i64, output: i64) -> f64;
}
```

**内置默认表**（USD/token，按 1e-6 = $/M token 写）：

| model key | input | output | cache_read | cache_creation |
|---|---|---|---|---|
| `claude-opus-4` | 15/M | 75/M | 1.5/M | 18.75/M |
| `claude-sonnet-4` | 3/M | 15/M | 0.3/M | 3.75/M |
| `claude-haiku-4-5` | 1/M | 5/M | 0.1/M | 1.25/M |

**model 归一化规则**（`price_for` 内部）：
1. 去掉 `-thinking` 后缀（thinking 不改变 token 计价）。
2. 前缀匹配到价格表 key（`claude-opus-4-7` / `claude-opus-4-6` → `claude-opus-4`）。
3. 未命中任何 key → fallback（取 sonnet 档，避免低估）。

**config.json 可选覆盖**：`Config` 新增可选字段

```jsonc
"pricing": {
  "claude-opus-4-7": { "input": 0.000015, "output": 0.000075, "cacheRead": 0.0000015, "cacheCreation": 0.00001875 }
}
```

- 缺省（无 `pricing` 节点）→ 纯内置表。
- 有 `pricing` → 覆盖/新增对应 key。
- 对应 sub2api 的"动态价 + 硬编码兜底"哲学，落到本项目的文件配置形态。

## 数据流改动（金额归因）

核心约束：**最终 usage 在 anthropic 层算定，credential_id 在 provider 层已知**。需把 id 从 provider 透传到 anthropic 层的 usage 终点。

1. **`src/kiro/provider.rs`**：`call_api_with_retry` 成功分支把 `ctx.id` 一并返回；公开方法 `call_api` / `call_api_stream`（以及 `call_mcp` 视需要）返回类型改为携带 credential_id 的小结构体或 `(reqwest::Response, u64)`。
   - MCP 路径（WebSearch）不参与本次计费归因，保持现状或返回 id 但不计费，避免扩散改动。
2. **`src/anthropic/handlers.rs`**：把 credential_id 透传给 `handle_stream_request` → `StreamContext`，以及 `handle_non_stream_request`。
3. **usage 终点记账**：
   - 流式：`StreamContext::generate_final_events` 算出 `(input, cache_creation, cache_read, output)` 后，用 `model` + credential_id 调 `token_manager.add_cost(...)`。
   - 非流式：在最终 usage 算定处同样调用。
   - `StreamContext` 新增 `credential_id: Option<u64>` 与 `pricing`/`token_manager` 句柄（按现有依赖注入方式传入；优先复用已可达的 `state` / `provider`）。

## 持久化（`kiro_stats.json`）

`CredentialEntry`（运行态）与 `StatsEntry`（持久化）各新增字段：

```rust
cost_usd: f64,                  // 累计金额（USD），列的数据源
input_tokens_total: u64,        // 累计输入 token（tooltip / 审计）
cache_read_tokens_total: u64,
cache_creation_tokens_total: u64,
output_tokens_total: u64,
```

- 新增方法 `MultiTokenManager::add_cost(id, model, input, cache_read, cache_creation, output)`：在写锁内累加 token 明细与金额（金额用 `PricingTable::cost_usd` 算），随后 `save_stats_debounced()`。
- `save_stats` / `load_stats` 同步新字段；旧 `kiro_stats.json` 缺字段时 serde `#[serde(default)]` 兜底为 0，**向后兼容**。
- `PricingTable` 在 `MultiTokenManager` 构造时从 `Config` 装配一次并持有（`Arc` 或值），`add_cost` 复用。

## Admin API + UI

- **`src/kiro/token_manager.rs::CredentialEntrySnapshot`** 与 **`src/admin/types.rs::CredentialStatusItem`** 新增：
  - `cost_usd: f64`
  - `input_tokens_total` / `cache_read_tokens_total` / `cache_creation_tokens_total` / `output_tokens_total`（`u64`）
- **`src/admin/service.rs`**：快照映射时带上新字段。
- **`admin-ui/src/types/api.ts`**：`CredentialStatusItem` 加对应字段（camelCase）。
- **`admin-ui/src/components/dashboard.tsx`**：表头在「计数 / 用量」与「最近使用」之间新增一列 **「消耗金额」**。
- **`admin-ui/src/components/credential-row.tsx`**：新增单元格
  - 主体显示 `$X.XXXX`（金额小、保留 4 位小数；`$0.0000` 表示未消耗）。
  - hover tooltip 展示 input / output / cache_read / cache_creation 累计 token 数，并注明"按各模型当时单价累计"。

## 测试

- **`src/pricing.rs` 单测**：
  - cache 倍率精算（构造已知 token，验证金额）。
  - model 归一化（`-thinking` 后缀、`opus-4-7`/`opus-4-6` 命中 `claude-opus-4`）。
  - 未知 model 走 fallback。
  - config 覆盖生效。
- **`token_manager` 单测**：`add_cost` 多次累加正确；`save_stats`/`load_stats` 往返保留金额与 token 明细；旧文件（缺字段）加载默认 0。

## 不做（YAGNI）

- 不做金额重置 / 按计费周期分桶（用户确认永久累计）。
- 不做运行时远程价格同步（仅内置表 + config 覆盖）。
- 不改 MCP/WebSearch 计费归因（量小，避免扩散）。
- 不引入货币换算（统一 USD）。
