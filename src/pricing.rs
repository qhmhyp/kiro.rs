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
