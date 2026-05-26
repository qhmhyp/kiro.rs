# 凭据 Token 消耗金额列 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在管理员页面为每个凭据新增一列，展示按真实 token × 模型单价累计的消耗金额（USD）。

**Architecture:** 新增 `pricing` 模块持有按模型的单价表（内置默认 + config.json 可选覆盖）。`MultiTokenManager` 为每个凭据累计 `cost_usd` 与 token 明细并持久化到 `kiro_stats.json`。Provider 在成功时把 credential_id 透传回 anthropic 层；流式在 `StreamContext::generate_final_events`、非流式在 handler 终点处算定最终 usage 后调用 `add_cost` 归因金额。金额经 snapshot → admin API → admin-ui 暴露为新列。

**Tech Stack:** Rust（axum / serde / tokio）、React + TypeScript（admin-ui，pnpm + vite + tsc）。

参考 spec：`docs/superpowers/specs/2026-05-26-credential-token-cost-column-design.md`

---

## 文件结构

| 文件 | 职责 | 动作 |
|---|---|---|
| `src/pricing.rs` | 模型单价表 + 计费纯函数 | 新建 |
| `src/main.rs` | 注册 `mod pricing;` | 修改 |
| `src/model/config.rs` | 新增可选 `pricing` 覆盖字段 | 修改 |
| `src/kiro/token_manager.rs` | 凭据累计金额/token、`add_cost`、持久化、snapshot 暴露、`PricingTable` 字段 | 修改 |
| `src/kiro/provider.rs` | 成功时返回 credential_id；暴露 `token_manager()` | 修改 |
| `src/anthropic/stream.rs` | `StreamContext` 计费 sink + 记账 | 修改 |
| `src/anthropic/handlers.rs` | 3 条响应路径透传 credential_id 并触发记账 | 修改 |
| `src/test.rs` | 适配 `call_api_stream` 新返回类型 | 修改 |
| `src/admin/types.rs` | `CredentialStatusItem` 新增金额字段 | 修改 |
| `src/admin/service.rs` | snapshot → status 映射带上金额字段 | 修改 |
| `admin-ui/src/types/api.ts` | 前端类型新增字段 | 修改 |
| `admin-ui/src/components/dashboard.tsx` | 表头新增「消耗金额」列 | 修改 |
| `admin-ui/src/components/credential-row.tsx` | 金额单元格 + tooltip | 修改 |

---

## Task 1: pricing 模块（单价表 + 计费纯函数）

**Files:**
- Create: `src/pricing.rs`
- Modify: `src/main.rs:8`（在 `pub mod token;` 后加 `mod pricing;`）

- [ ] **Step 1: 写失败测试**

把以下内容写入 `src/pricing.rs`（仅测试 + 待实现的类型签名，先让它编译失败）：

```rust
//! 模型 token 单价表与计费纯函数
//!
//! 参考 sub2api 的 LiteLLM 式 schema：每个模型有 input/output/cache_read/cache_creation
//! 四档 USD/token 单价。cache 按 Anthropic 倍率（cache_read≈0.1×input，
//! cache_creation≈1.25×input）。内置默认表 + config.json 可选覆盖。

use serde::Deserialize;
use std::collections::HashMap;

/// 单个模型的单价（USD / token）
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModelPrice {
    pub input_cost_per_token: f64,
    pub output_cost_per_token: f64,
    pub cache_read_input_token_cost: f64,
    pub cache_creation_input_token_cost: f64,
}

/// config.json 中的单价覆盖项（camelCase）
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelPriceConfig {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_creation: f64,
}

/// 价格表：按归一化 model key 查价，未命中走 fallback
#[derive(Debug, Clone)]
pub struct PricingTable {
    table: HashMap<String, ModelPrice>,
    fallback: ModelPrice,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builtin_opus_cost_with_cache_multipliers() {
        let t = PricingTable::builtin();
        // opus: input 15/M, output 75/M, cache_read 1.5/M, cache_creation 18.75/M
        // 1M input + 1M output + 1M cache_read + 1M cache_creation
        let cost = t.cost_usd("claude-opus-4-7", 1_000_000, 1_000_000, 1_000_000, 1_000_000);
        // 15 + 75 + 1.5 + 18.75 = 110.25
        assert!((cost - 110.25).abs() < 1e-6, "got {cost}");
    }

    #[test]
    fn test_thinking_suffix_normalized() {
        let t = PricingTable::builtin();
        let a = t.price_for("claude-opus-4-6-thinking");
        let b = t.price_for("claude-opus-4-6");
        assert_eq!(a, b);
    }

    #[test]
    fn test_opus_46_and_47_share_prefix() {
        let t = PricingTable::builtin();
        assert_eq!(t.price_for("claude-opus-4-7"), t.price_for("claude-opus-4-6"));
    }

    #[test]
    fn test_unknown_model_uses_fallback_sonnet() {
        let t = PricingTable::builtin();
        // 未知模型 → fallback（sonnet 档）：1M input = $3
        let cost = t.cost_usd("some-unknown-model", 1_000_000, 0, 0, 0);
        assert!((cost - 3.0).abs() < 1e-6, "got {cost}");
    }

    #[test]
    fn test_config_override_exact_key_wins() {
        let mut overrides = HashMap::new();
        overrides.insert(
            "claude-opus-4-7".to_string(),
            ModelPriceConfig { input: 0.001, output: 0.002, cache_read: 0.0001, cache_creation: 0.003 },
        );
        let t = PricingTable::builtin().with_overrides(Some(&overrides));
        let cost = t.cost_usd("claude-opus-4-7", 1000, 0, 0, 0);
        assert!((cost - 1.0).abs() < 1e-9, "got {cost}"); // 1000 * 0.001
        // 未覆盖的 opus-4-6 仍走内置前缀价
        assert_eq!(t.price_for("claude-opus-4-6"), &ModelPrice {
            input_cost_per_token: 15.0 / 1e6,
            output_cost_per_token: 75.0 / 1e6,
            cache_read_input_token_cost: 1.5 / 1e6,
            cache_creation_input_token_cost: 18.75 / 1e6,
        });
    }

    #[test]
    fn test_negative_tokens_clamped() {
        let t = PricingTable::builtin();
        assert_eq!(t.cost_usd("claude-sonnet-4-6", -5, -5, -5, -5), 0.0);
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --lib pricing 2>&1 | head -30`（或 `cargo test pricing`）
Expected: 编译失败 —— `PricingTable::builtin` / `cost_usd` / `price_for` / `with_overrides` 未实现。

- [ ] **Step 3: 实现 PricingTable**

在 `src/pricing.rs` 的 `mod tests` **之前**插入实现：

```rust
/// 每 token 单价 = 每百万 token 价 / 1e6
const PER_M: f64 = 1e6;

impl PricingTable {
    /// 内置默认表（Anthropic Claude 4 系列挂牌价，USD/M token）
    pub fn builtin() -> Self {
        let opus = ModelPrice {
            input_cost_per_token: 15.0 / PER_M,
            output_cost_per_token: 75.0 / PER_M,
            cache_read_input_token_cost: 1.5 / PER_M,
            cache_creation_input_token_cost: 18.75 / PER_M,
        };
        let sonnet = ModelPrice {
            input_cost_per_token: 3.0 / PER_M,
            output_cost_per_token: 15.0 / PER_M,
            cache_read_input_token_cost: 0.3 / PER_M,
            cache_creation_input_token_cost: 3.75 / PER_M,
        };
        let haiku = ModelPrice {
            input_cost_per_token: 1.0 / PER_M,
            output_cost_per_token: 5.0 / PER_M,
            cache_read_input_token_cost: 0.1 / PER_M,
            cache_creation_input_token_cost: 1.25 / PER_M,
        };
        let mut table = HashMap::new();
        table.insert("claude-opus-4".to_string(), opus);
        table.insert("claude-sonnet-4".to_string(), sonnet);
        table.insert("claude-haiku-4-5".to_string(), haiku);
        // 未知模型兜底取 sonnet 档（避免低估）
        Self { table, fallback: sonnet }
    }

    /// 应用 config 覆盖（按 model 原始 key 精确插入；None 时原样返回）
    pub fn with_overrides(mut self, overrides: Option<&HashMap<String, ModelPriceConfig>>) -> Self {
        if let Some(map) = overrides {
            for (k, v) in map {
                self.table.insert(
                    k.to_ascii_lowercase(),
                    ModelPrice {
                        input_cost_per_token: v.input,
                        output_cost_per_token: v.output,
                        cache_read_input_token_cost: v.cache_read,
                        cache_creation_input_token_cost: v.cache_creation,
                    },
                );
            }
        }
        self
    }

    /// 归一化 model：转小写并去掉 `-thinking` 后缀
    fn normalize(model: &str) -> String {
        let lower = model.to_ascii_lowercase();
        match lower.strip_suffix("-thinking") {
            Some(s) => s.to_string(),
            None => lower,
        }
    }

    /// 查单价：先精确命中（含 config 覆盖的全量 key），再按最长前缀命中内置 key，最后 fallback
    pub fn price_for(&self, model: &str) -> &ModelPrice {
        let norm = Self::normalize(model);
        if let Some(p) = self.table.get(&norm) {
            return p;
        }
        let mut best: Option<(&String, &ModelPrice)> = None;
        for (k, v) in &self.table {
            if norm.starts_with(k.as_str()) {
                if best.map_or(true, |(bk, _)| k.len() > bk.len()) {
                    best = Some((k, v));
                }
            }
        }
        best.map(|(_, v)| v).unwrap_or(&self.fallback)
    }

    /// 计费纯函数（负数 token 视为 0）
    pub fn cost_usd(
        &self,
        model: &str,
        input: i64,
        cache_read: i64,
        cache_creation: i64,
        output: i64,
    ) -> f64 {
        let p = self.price_for(model);
        let clamp = |n: i64| n.max(0) as f64;
        clamp(input) * p.input_cost_per_token
            + clamp(cache_read) * p.cache_read_input_token_cost
            + clamp(cache_creation) * p.cache_creation_input_token_cost
            + clamp(output) * p.output_cost_per_token
    }
}
```

然后在 `src/main.rs` 第 8 行 `pub mod token;` 之后新增一行：

```rust
mod pricing;
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test pricing 2>&1 | tail -20`
Expected: 6 个 pricing 测试全部 PASS。

- [ ] **Step 5: 提交**

```bash
git add src/pricing.rs src/main.rs
git commit -m "feat: 新增 pricing 模块（模型单价表 + 计费纯函数）

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 2: Config 新增可选 pricing 覆盖字段

**Files:**
- Modify: `src/model/config.rs`（`use` 区、`Config` 结构体、`Default` 实现）

- [ ] **Step 1: 在 config.rs 顶部引入类型**

`src/model/config.rs` 第 1-2 行当前为：

```rust
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
```

在其后新增一行：

```rust
use crate::pricing::ModelPriceConfig;
```

- [ ] **Step 2: 在 Config 结构体新增字段**

在 `src/model/config.rs` 的 `endpoints` 字段（约 108-109 行）之后、`config_path` 字段之前插入：

```rust
    /// 可选的模型单价覆盖表（key=model id，覆盖/新增内置价格表条目）
    /// 缺省时使用内置默认价。
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pricing: Option<HashMap<String, ModelPriceConfig>>,
```

- [ ] **Step 3: 在 Default 实现补字段**

在 `impl Default for Config` 的 `endpoints: HashMap::new(),`（约 185 行）之后插入：

```rust
            pricing: None,
```

- [ ] **Step 4: 编译确认通过**

Run: `cargo build 2>&1 | tail -20`
Expected: 编译成功（`ModelPriceConfig` 已在 Task 1 定义并 `pub`）。

- [ ] **Step 5: 提交**

```bash
git add src/model/config.rs
git commit -m "feat: config.json 支持可选 pricing 单价覆盖

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 3: 凭据累计金额/token 字段 + add_cost + 持久化

**Files:**
- Modify: `src/kiro/token_manager.rs`（`CredentialEntry`、`StatsEntry`、`MultiTokenManager` 字段、`new`、3 处 `CredentialEntry {` 构造、`load_stats`、`save_stats`、新增 `add_cost`）

- [ ] **Step 1: 写失败测试**

在 `src/kiro/token_manager.rs` 的 `mod tests` 内（紧接 `test_multi_token_manager_report_success` 之后，约第 2300 行）插入：

```rust
    #[test]
    fn test_add_cost_accumulates() {
        let config = Config::default();
        let cred = KiroCredentials::default();
        let manager = MultiTokenManager::new(config, vec![cred], None, None, false).unwrap();

        // sonnet: input 3/M, output 15/M。1000 input + 1000 output
        manager.add_cost(1, "claude-sonnet-4-6", 1000, 0, 0, 1000);
        // 累计第二次
        manager.add_cost(1, "claude-sonnet-4-6", 1000, 0, 0, 1000);

        let snap = manager.snapshot();
        let entry = snap.entries.iter().find(|e| e.id == 1).unwrap();
        // 每次 = 1000*3e-6 + 1000*15e-6 = 0.003 + 0.015 = 0.018；两次 = 0.036
        assert!((entry.cost_usd - 0.036).abs() < 1e-9, "got {}", entry.cost_usd);
        assert_eq!(entry.input_tokens_total, 2000);
        assert_eq!(entry.output_tokens_total, 2000);
    }

    #[test]
    fn test_add_cost_unknown_id_noop() {
        let config = Config::default();
        let cred = KiroCredentials::default();
        let manager = MultiTokenManager::new(config, vec![cred], None, None, false).unwrap();
        manager.add_cost(999, "claude-sonnet-4-6", 1000, 0, 0, 1000); // 不存在的 id
        let snap = manager.snapshot();
        assert_eq!(snap.entries[0].cost_usd, 0.0);
    }

    #[test]
    fn test_stats_entry_backward_compat_defaults() {
        // 旧格式 kiro_stats.json（无金额/token 字段）应反序列化为默认 0，保证向后兼容
        let json = r#"{"4":{"success_count":11,"last_used_at":"2026-05-15T05:25:33Z"}}"#;
        let stats: std::collections::HashMap<String, StatsEntry> =
            serde_json::from_str(json).unwrap();
        let s = stats.get("4").unwrap();
        assert_eq!(s.success_count, 11);
        assert_eq!(s.cost_usd, 0.0);
        assert_eq!(s.input_tokens_total, 0);
        assert_eq!(s.output_tokens_total, 0);
    }
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test test_add_cost 2>&1 | head -20`
Expected: 编译失败 —— `add_cost` 未定义、`CredentialEntrySnapshot` 无 `cost_usd` 字段。

- [ ] **Step 3a: 给 CredentialEntry 加字段**

在 `src/kiro/token_manager.rs` 的 `struct CredentialEntry` 中，`last_error: Option<RecentError>,`（约第 420 行）之后插入：

```rust
    /// 累计消耗金额（USD）
    cost_usd: f64,
    /// 累计输入 token
    input_tokens_total: u64,
    /// 累计 cache_read token
    cache_read_tokens_total: u64,
    /// 累计 cache_creation token
    cache_creation_tokens_total: u64,
    /// 累计输出 token
    output_tokens_total: u64,
```

- [ ] **Step 3b: 给 StatsEntry 加字段**

把 `struct StatsEntry`（约第 476-480 行）替换为：

```rust
/// 统计数据持久化条目
#[derive(Serialize, Deserialize)]
struct StatsEntry {
    success_count: u64,
    last_used_at: Option<String>,
    #[serde(default)]
    cost_usd: f64,
    #[serde(default)]
    input_tokens_total: u64,
    #[serde(default)]
    cache_read_tokens_total: u64,
    #[serde(default)]
    cache_creation_tokens_total: u64,
    #[serde(default)]
    output_tokens_total: u64,
}
```

- [ ] **Step 3c: 给 CredentialEntrySnapshot 加字段**

在 `pub struct CredentialEntrySnapshot` 的 `last_error: Option<RecentError>,`（约第 537 行）之后插入：

```rust
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
```

- [ ] **Step 3d: MultiTokenManager 增加 pricing 字段**

在 `pub struct MultiTokenManager` 的 `stats_dirty: AtomicBool,`（约第 574 行）之后插入：

```rust
    /// 模型单价表（内置默认 + config 覆盖），构造时装配一次
    pricing: crate::pricing::PricingTable,
```

- [ ] **Step 3e: 在 new() 构造 pricing 并填进 manager**

在 `src/kiro/token_manager.rs` 的 `let manager = Self {`（约第 707 行）**之前**插入一行（此时 `config` 尚未被 move，可直接借用）：

```rust
        let pricing = crate::pricing::PricingTable::builtin()
            .with_overrides(config.pricing.as_ref());
```

然后在 `let manager = Self {` 块中 `stats_dirty: AtomicBool::new(false),` 之后插入：

```rust
            pricing,
```

（结构体字面量中 `config` 字段在前先 move `config`，但 `pricing` 已是上面算好的独立局部变量，不再依赖 `config`，无借用冲突。避免克隆整个 `Config`。）

- [ ] **Step 3f: 更新主构造点 CredentialEntry（约第 646-661 行）**

在该 `CredentialEntry {` 字面量的 `last_error: None,` 之后插入：

```rust
                    cost_usd: 0.0,
                    input_tokens_total: 0,
                    cache_read_tokens_total: 0,
                    cache_creation_tokens_total: 0,
                    output_tokens_total: 0,
```

- [ ] **Step 3g: 更新两个测试构造点 CredentialEntry（约第 1867 行、第 2485 行）**

这两处 `entries.push(CredentialEntry { ... last_used_at: None, ... })` 同样在其末尾字段后补：

```rust
                cost_usd: 0.0,
                input_tokens_total: 0,
                cache_read_tokens_total: 0,
                cache_creation_tokens_total: 0,
                output_tokens_total: 0,
```

（缩进按各自上下文对齐；用 `cargo build` 的报错定位精确行。）

- [ ] **Step 3h: load_stats 读入新字段**

把 `load_stats` 中应用统计的循环体（约第 1090-1094 行）：

```rust
        for entry in entries.iter_mut() {
            if let Some(s) = stats.get(&entry.id.to_string()) {
                entry.success_count = s.success_count;
                entry.last_used_at = s.last_used_at.clone();
            }
        }
```

替换为：

```rust
        for entry in entries.iter_mut() {
            if let Some(s) = stats.get(&entry.id.to_string()) {
                entry.success_count = s.success_count;
                entry.last_used_at = s.last_used_at.clone();
                entry.cost_usd = s.cost_usd;
                entry.input_tokens_total = s.input_tokens_total;
                entry.cache_read_tokens_total = s.cache_read_tokens_total;
                entry.cache_creation_tokens_total = s.cache_creation_tokens_total;
                entry.output_tokens_total = s.output_tokens_total;
            }
        }
```

- [ ] **Step 3i: save_stats 写出新字段**

把 `save_stats` 中构造 `StatsEntry` 的闭包（约第 1112-1119 行）：

```rust
                        StatsEntry {
                            success_count: e.success_count,
                            last_used_at: e.last_used_at.clone(),
                        },
```

替换为：

```rust
                        StatsEntry {
                            success_count: e.success_count,
                            last_used_at: e.last_used_at.clone(),
                            cost_usd: e.cost_usd,
                            input_tokens_total: e.input_tokens_total,
                            cache_read_tokens_total: e.cache_read_tokens_total,
                            cache_creation_tokens_total: e.cache_creation_tokens_total,
                            output_tokens_total: e.output_tokens_total,
                        },
```

- [ ] **Step 3j: 新增 add_cost 方法**

在 `report_success` 方法（约第 1178 行 `}` 结束）之后插入：

```rust
    /// 累计指定凭据的 token 消耗金额（USD）与 token 明细
    ///
    /// 在 anthropic 层算定最终 usage 后调用。金额按记录时刻生效的单价算定累加；
    /// token 负值按 0 处理。与 [`Self::report_success`] 解耦：成功计数在 provider 层
    /// 已记，本方法只负责金额/明细。
    ///
    /// # Arguments
    /// * `id` - 凭据 ID
    /// * `model` - 本次请求模型 ID
    /// * `input` / `cache_read` / `cache_creation` / `output` - 本次 usage 四段 token
    pub fn add_cost(
        &self,
        id: u64,
        model: &str,
        input: i32,
        cache_read: i32,
        cache_creation: i32,
        output: i32,
    ) {
        let cost = self.pricing.cost_usd(
            model,
            input as i64,
            cache_read as i64,
            cache_creation as i64,
            output as i64,
        );
        {
            let mut entries = self.entries.lock();
            if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
                entry.cost_usd += cost;
                entry.input_tokens_total += input.max(0) as u64;
                entry.cache_read_tokens_total += cache_read.max(0) as u64;
                entry.cache_creation_tokens_total += cache_creation.max(0) as u64;
                entry.output_tokens_total += output.max(0) as u64;
                tracing::debug!(
                    "凭据 #{} 记账 +${:.6}（model={}, in={}, cr={}, cc={}, out={}），累计 ${:.6}",
                    id, cost, model, input, cache_read, cache_creation, output, entry.cost_usd
                );
            }
        }
        self.save_stats_debounced();
    }
```

- [ ] **Step 3k: snapshot() 映射新字段**

在 `snapshot()` 的 `CredentialEntrySnapshot { ... }` 字面量（约第 1454-1509 行）末尾 `last_error: e.last_error.clone(),` 之后插入：

```rust
                    cost_usd: e.cost_usd,
                    input_tokens_total: e.input_tokens_total,
                    cache_read_tokens_total: e.cache_read_tokens_total,
                    cache_creation_tokens_total: e.cache_creation_tokens_total,
                    output_tokens_total: e.output_tokens_total,
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test test_add_cost 2>&1 | tail -20` 然后 `cargo test test_stats_entry_backward_compat 2>&1 | tail -10` 然后 `cargo test --lib token_manager 2>&1 | tail -20`
Expected: 新增 3 个测试（2×add_cost + backward_compat）PASS，token_manager 既有测试仍全部 PASS。

- [ ] **Step 5: 提交**

```bash
git add src/kiro/token_manager.rs
git commit -m "feat: 凭据累计消耗金额/token + add_cost + 持久化

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 4: Provider 成功时返回 credential_id + 暴露 token_manager()

**Files:**
- Modify: `src/kiro/provider.rs`（`call_api`、`call_api_stream`、`call_api_with_retry` 返回类型，新增 `token_manager()`）
- Modify: `src/test.rs:50`（适配新返回类型）

- [ ] **Step 1: 改 call_api_with_retry 返回类型与成功分支**

`src/kiro/provider.rs` 约第 529-533 行签名：

```rust
    async fn call_api_with_retry(
        &self,
        request_body: &str,
        is_stream: bool,
    ) -> anyhow::Result<reqwest::Response> {
```

改为：

```rust
    async fn call_api_with_retry(
        &self,
        request_body: &str,
        is_stream: bool,
    ) -> anyhow::Result<(reqwest::Response, u64)> {
```

并把成功分支（约第 605-609 行）：

```rust
            // 成功响应
            if status.is_success() {
                self.token_manager.report_success(ctx.id);
                return Ok(response);
            }
```

改为：

```rust
            // 成功响应
            if status.is_success() {
                self.token_manager.report_success(ctx.id);
                return Ok((response, ctx.id));
            }
```

- [ ] **Step 2: 改 call_api / call_api_stream 返回类型**

`src/kiro/provider.rs` 约第 227 行：

```rust
    pub async fn call_api(&self, request_body: &str) -> anyhow::Result<reqwest::Response> {
        self.call_api_with_retry(request_body, false).await
    }
```

改为：

```rust
    pub async fn call_api(&self, request_body: &str) -> anyhow::Result<(reqwest::Response, u64)> {
        self.call_api_with_retry(request_body, false).await
    }
```

约第 333 行：

```rust
    pub async fn call_api_stream(&self, request_body: &str) -> anyhow::Result<reqwest::Response> {
        self.call_api_with_retry(request_body, true).await
    }
```

改为：

```rust
    pub async fn call_api_stream(&self, request_body: &str) -> anyhow::Result<(reqwest::Response, u64)> {
        self.call_api_with_retry(request_body, true).await
    }
```

（`call_mcp` / `call_mcp_with_retry` 保持不变 —— MCP 不计费。）

- [ ] **Step 3: 新增 token_manager() getter**

在 `call_api`（约第 229 行 `}` 之后）插入：

```rust
    /// 暴露底层 token manager（供 anthropic 层记账金额）
    pub fn token_manager(&self) -> std::sync::Arc<MultiTokenManager> {
        self.token_manager.clone()
    }
```

- [ ] **Step 4: 适配 src/test.rs 调用点**

`src/test.rs:50` 当前为：

```rust
    let response = provider.call_api_stream(&request_body).await?;
```

改为：

```rust
    let (response, _credential_id) = provider.call_api_stream(&request_body).await?;
```

- [ ] **Step 5: 编译确认（handlers.rs 会报错，符合预期）**

Run: `cargo build 2>&1 | grep -A2 "error\[" | head -40`
Expected: 仅 `src/anthropic/handlers.rs` 的 3 处调用点因元组解构未适配而报错（449/589/1001 行附近）。Task 7 修复。`provider.rs`、`test.rs` 不应再报错。

- [ ] **Step 6: 提交（带 WIP 标记，因为 handlers 尚未适配）**

```bash
git add src/kiro/provider.rs src/test.rs
git commit -m "feat: provider 成功时返回 credential_id + 暴露 token_manager()

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 5: StreamContext 计费 sink + 记账

**Files:**
- Modify: `src/anthropic/stream.rs`（`use` 区、`StreamContext` 字段与构造、`with_cost_sink`、`generate_final_events`、`BufferedStreamContext::with_cost_sink`）

- [ ] **Step 1: 写失败测试**

在 `src/anthropic/stream.rs` 文件末尾的 `#[cfg(test)] mod tests` 中（若无则新建一个 `#[cfg(test)] mod cost_tests { ... }`）追加：

```rust
#[cfg(test)]
mod cost_recording_tests {
    use super::*;
    use crate::kiro::model::credentials::KiroCredentials;
    use crate::kiro::token_manager::MultiTokenManager;
    use crate::model::config::Config;
    use std::sync::Arc;

    #[test]
    fn test_stream_final_events_record_cost() {
        let manager = Arc::new(
            MultiTokenManager::new(Config::default(), vec![KiroCredentials::default()], None, None, false).unwrap(),
        );
        let mut ctx = StreamContext::new_with_thinking(
            "claude-sonnet-4-6",
            1000, // input_tokens
            false,
            std::collections::HashMap::new(),
        )
        .with_cost_sink(manager.clone(), 1);
        ctx.output_tokens = 500;

        // 无 usage_cache 时拆分 = (0, 0, 1000)
        let _ = ctx.generate_final_events();

        let snap = manager.snapshot();
        let e = snap.entries.iter().find(|e| e.id == 1).unwrap();
        // sonnet: 1000 input * 3e-6 + 500 output * 15e-6 = 0.003 + 0.0075 = 0.0105
        assert!((e.cost_usd - 0.0105).abs() < 1e-9, "got {}", e.cost_usd);

        // 二次调用不应重复记账（cost_recorded 保护）
        let _ = ctx.generate_final_events();
        let snap2 = manager.snapshot();
        let e2 = snap2.entries.iter().find(|e| e.id == 1).unwrap();
        assert!((e2.cost_usd - 0.0105).abs() < 1e-9, "double-counted: {}", e2.cost_usd);
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test cost_recording 2>&1 | head -20`
Expected: 编译失败 —— `with_cost_sink` 未定义。

- [ ] **Step 3a: 引入依赖类型**

在 `src/anthropic/stream.rs` 顶部 `use super::prefix_cache::ConvoTokenCache;`（约第 11 行）之后插入：

```rust
use crate::kiro::token_manager::MultiTokenManager;
```

并在文件已有 `use std::sync::Arc;`（若没有则新增）。确认 `Arc` 已在作用域（`UsageCacheCtx` 已用 `Arc`，所以已 import）。

- [ ] **Step 3b: 定义 CostSink 并加到 StreamContext**

在 `pub struct StreamContext {`（约第 534 行）之前插入：

```rust
/// 金额记账 sink：流结束算定 usage 后把消耗金额累计到该凭据
pub struct CostSink {
    pub manager: Arc<MultiTokenManager>,
    pub credential_id: u64,
}
```

在 `StreamContext` 的 `final_usage_split: Option<(i32, i32, i32)>,`（约第 571 行）之后插入：

```rust
    /// 可选的金额记账 sink（注入后 generate_final_events 会记一次账）
    pub cost_sink: Option<CostSink>,
    /// 金额是否已记账（防止重复记账）
    pub cost_recorded: bool,
```

- [ ] **Step 3c: 构造函数初始化新字段**

在 `new_with_thinking` 的返回 `Self { ... }`（约第 582-600 行）中，`final_usage_split: None,` 之后插入：

```rust
            cost_sink: None,
            cost_recorded: false,
```

- [ ] **Step 3d: 新增 with_cost_sink 构造器**

在 `with_usage_cache` 方法（约第 616 行 `}` 结束）之后插入：

```rust
    /// 注入金额记账 sink（构建器方法）
    pub fn with_cost_sink(mut self, manager: Arc<MultiTokenManager>, credential_id: u64) -> Self {
        self.cost_sink = Some(CostSink { manager, credential_id });
        self
    }
```

- [ ] **Step 3e: 在 generate_final_events 末尾记账**

在 `StreamContext::generate_final_events`（约第 1105 行）末尾，`events` 返回之前（约第 1200-1201 行，`events.extend(...)` 之后、`events` 这一行之前）插入：

```rust
        // 记账：把本次 usage 折算金额累计到凭据（仅一次）
        if !self.cost_recorded {
            if let Some(sink) = &self.cost_sink {
                sink.manager.add_cost(
                    sink.credential_id,
                    &self.model,
                    input_tokens_split,
                    cache_read,
                    cache_creation,
                    self.output_tokens,
                );
            }
            self.cost_recorded = true;
        }
```

（此处 `input_tokens_split` / `cache_read` / `cache_creation` 是同方法内约第 1185 行算出的局部变量，仍在作用域。）

- [ ] **Step 3f: BufferedStreamContext 透传 with_cost_sink**

在 `impl BufferedStreamContext` 的 `with_usage_cache`（约第 1255 行 `}` 结束）之后插入：

```rust
    /// 注入金额记账 sink（透传到内部 StreamContext）
    pub fn with_cost_sink(mut self, manager: Arc<MultiTokenManager>, credential_id: u64) -> Self {
        self.inner = self.inner.with_cost_sink(manager, credential_id);
        self
    }
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test cost_recording 2>&1 | tail -20`
Expected: `test_stream_final_events_record_cost` PASS。

- [ ] **Step 5: 提交**

```bash
git add src/anthropic/stream.rs
git commit -m "feat: StreamContext 计费 sink，流结束记账消耗金额

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 6: handlers.rs 三条响应路径接线

**Files:**
- Modify: `src/anthropic/handlers.rs`（`handle_stream_request` 449、`handle_non_stream_request` 589/747、`handle_stream_request_buffered` 1001/1007）

- [ ] **Step 1: 流式路径（handle_stream_request）**

`src/anthropic/handlers.rs` 约第 449-456 行：

```rust
    let response = match provider.call_api_stream(request_body).await {
        Ok(resp) => resp,
        Err(e) => return map_provider_error(e),
    };

    // 创建流处理上下文
    let mut ctx = StreamContext::new_with_thinking(model, input_tokens, thinking_enabled, tool_name_map)
        .with_usage_cache(convo_cache, conversation_id, current_turn_tokens);
```

替换为：

```rust
    let (response, credential_id) = match provider.call_api_stream(request_body).await {
        Ok(resp) => resp,
        Err(e) => return map_provider_error(e),
    };

    // 创建流处理上下文
    let mut ctx = StreamContext::new_with_thinking(model, input_tokens, thinking_enabled, tool_name_map)
        .with_usage_cache(convo_cache, conversation_id, current_turn_tokens)
        .with_cost_sink(provider.token_manager(), credential_id);
```

- [ ] **Step 2: 非流式路径（handle_non_stream_request）—— 解构返回值**

约第 589-592 行：

```rust
    let response = match provider.call_api(request_body).await {
        Ok(resp) => resp,
        Err(e) => return map_provider_error(e),
    };
```

替换为：

```rust
    let (response, credential_id) = match provider.call_api(request_body).await {
        Ok(resp) => resp,
        Err(e) => return map_provider_error(e),
    };
```

- [ ] **Step 3: 非流式路径 —— 算定 usage 后记账**

约第 749 行 `convo_cache.commit(&conversation_id, final_input_tokens, current_turn_tokens);` 之后、构建响应体之前插入：

```rust
    // 记账：折算本次 usage 金额累计到凭据
    provider.token_manager().add_cost(
        credential_id,
        model,
        input_tokens_split,
        cache_read,
        cache_creation,
        output_tokens,
    );
```

- [ ] **Step 4: 缓冲流路径（handle_stream_request_buffered）**

约第 1001-1008 行：

```rust
    let response = match provider.call_api_stream(request_body).await {
        Ok(resp) => resp,
        Err(e) => return map_provider_error(e),
    };

    // 创建缓冲流处理上下文
    let ctx = BufferedStreamContext::new(model, estimated_input_tokens, thinking_enabled, tool_name_map)
        .with_usage_cache(convo_cache, conversation_id, current_turn_tokens);
```

替换为：

```rust
    let (response, credential_id) = match provider.call_api_stream(request_body).await {
        Ok(resp) => resp,
        Err(e) => return map_provider_error(e),
    };

    // 创建缓冲流处理上下文
    let ctx = BufferedStreamContext::new(model, estimated_input_tokens, thinking_enabled, tool_name_map)
        .with_usage_cache(convo_cache, conversation_id, current_turn_tokens)
        .with_cost_sink(provider.token_manager(), credential_id);
```

- [ ] **Step 5: 全量编译 + 测试**

Run: `cargo build 2>&1 | tail -20` 然后 `cargo test 2>&1 | tail -25`
Expected: 编译成功，无 error；全部既有测试 + 新增测试 PASS。

- [ ] **Step 6: clippy 检查**

Run: `cargo clippy 2>&1 | grep -E "warning|error" | head -20`
Expected: 无新增 warning（若有 `unused variable credential_id` 之类说明某路径漏接线，回查）。

- [ ] **Step 7: 提交**

```bash
git add src/anthropic/handlers.rs
git commit -m "feat: 三条响应路径透传 credential_id 并触发金额记账

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 7: Admin API 类型 + service 映射

**Files:**
- Modify: `src/admin/types.rs`（`CredentialStatusItem`）
- Modify: `src/admin/service.rs`（`get_all_credentials` 映射）

- [ ] **Step 1: CredentialStatusItem 新增字段**

`src/admin/types.rs` 的 `pub struct CredentialStatusItem` 中，`last_error: Option<...>,`（约第 73 行）之后插入：

```rust
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
```

- [ ] **Step 2: service 映射补字段**

`src/admin/service.rs` 的 `get_all_credentials` 中 `CredentialStatusItem { ... }` 字面量（约第 74-97 行）末尾 `last_error: entry.last_error,` 之后插入：

```rust
                cost_usd: entry.cost_usd,
                input_tokens_total: entry.input_tokens_total,
                cache_read_tokens_total: entry.cache_read_tokens_total,
                cache_creation_tokens_total: entry.cache_creation_tokens_total,
                output_tokens_total: entry.output_tokens_total,
```

- [ ] **Step 3: 编译 + 测试**

Run: `cargo build 2>&1 | tail -15` 然后 `cargo test 2>&1 | tail -15`
Expected: 编译成功，测试全过。

- [ ] **Step 4: 验证 JSON 输出含 camelCase 字段（可选 sanity check）**

`CredentialStatusItem` 已带 `#[serde(rename_all = "camelCase")]`，序列化后字段为 `costUsd` / `inputTokensTotal` 等。无需额外测试。

- [ ] **Step 5: 提交**

```bash
git add src/admin/types.rs src/admin/service.rs
git commit -m "feat: admin API 暴露凭据累计消耗金额与 token 明细

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 8: Admin UI 新增「消耗金额」列

**Files:**
- Modify: `admin-ui/src/types/api.ts`（`CredentialStatusItem`）
- Modify: `admin-ui/src/components/dashboard.tsx`（表头）
- Modify: `admin-ui/src/components/credential-row.tsx`（单元格）

- [ ] **Step 1: 前端类型新增字段**

`admin-ui/src/types/api.ts` 的 `interface CredentialStatusItem` 中，`lastError?: RecentError`（约第 32 行）之后插入：

```ts
  costUsd: number
  inputTokensTotal: number
  cacheReadTokensTotal: number
  cacheCreationTokensTotal: number
  outputTokensTotal: number
```

- [ ] **Step 2: 表头新增列**

`admin-ui/src/components/dashboard.tsx` 约第 735-736 行：

```tsx
                      <th className="px-2 py-2 text-left font-medium">计数 / 用量</th>
                      <th className="px-2 py-2 text-left font-medium">最近使用</th>
```

在两行之间插入：

```tsx
                      <th className="px-2 py-2 text-left font-medium">消耗金额</th>
```

得到顺序：计数/用量 → 消耗金额 → 最近使用。

- [ ] **Step 3: 在 credential-row.tsx 新增金额单元格**

`admin-ui/src/components/credential-row.tsx` 中「失败 / 用量」单元格结束 `</td>`（约第 404 行）之后、`{/* 最近使用 */}`（约第 406 行）之前，插入：

```tsx
        {/* 消耗金额 */}
        <td className="px-2 py-2 text-sm whitespace-nowrap">
          <span
            className="text-xs font-medium tabular-nums cursor-help"
            title={[
              `输入: ${credential.inputTokensTotal.toLocaleString()} tok`,
              `输出: ${credential.outputTokensTotal.toLocaleString()} tok`,
              `cache 读: ${credential.cacheReadTokensTotal.toLocaleString()} tok`,
              `cache 写: ${credential.cacheCreationTokensTotal.toLocaleString()} tok`,
              '（按各模型当时单价累计）',
            ].join('\n')}
          >
            ${credential.costUsd.toFixed(4)}
          </span>
        </td>
```

- [ ] **Step 4: 前端类型检查 + 构建**

Run: `cd admin-ui && pnpm build 2>&1 | tail -25`
Expected: `tsc -b` 无类型错误，`vite build` 成功产出。

- [ ] **Step 5: 提交**

```bash
git add admin-ui/src/types/api.ts admin-ui/src/components/dashboard.tsx admin-ui/src/components/credential-row.tsx
git commit -m "feat: 管理员页面新增凭据消耗金额列（含 token 明细 tooltip）

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 9: 文档与示例（可选但建议）

**Files:**
- Modify: `config.example.json`（演示可选 pricing 节点，注释说明）

- [ ] **Step 1: 在 config.example.json 加一条注释性示例**

由于 `config.example.json` 是纯 JSON（不支持注释），仅在 README 或 spec 已说明的前提下，此步可跳过。如需演示，可在 README.md 的配置说明处补一段：

````markdown
### 可选：自定义模型单价（用于"消耗金额"统计）

`config.json` 可加 `pricing` 节点覆盖内置单价（单位：USD / token）：

```json
{
  "pricing": {
    "claude-opus-4-7": {
      "input": 0.000015,
      "output": 0.000075,
      "cacheRead": 0.0000015,
      "cacheCreation": 0.00001875
    }
  }
}
```

缺省时使用内置默认价（Anthropic Claude 4 系列挂牌价）。未知模型按 Sonnet 档兜底。
````

- [ ] **Step 2: 提交**

```bash
git add README.md
git commit -m "docs: 说明 config.json 可选 pricing 单价覆盖

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## 最终验证

- [ ] `cargo test 2>&1 | tail -15` —— 全部 PASS
- [ ] `cargo clippy 2>&1 | grep -E "error|warning" | head` —— 无新增告警
- [ ] `cd admin-ui && pnpm build` —— 构建成功
- [ ] 手动（可选）：启动服务，发一次 `/v1/messages` 请求，刷新管理员页面确认对应凭据「消耗金额」列从 `$0.0000` 增长，tooltip 显示 token 明细；重启服务后金额仍保留（已落 `kiro_stats.json`）。
