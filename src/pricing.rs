//! 模型 token 单价表与计费纯函数
//!
//! 参考 sub2api 的 LiteLLM 式 schema：每个模型有 input/output/cache_read/cache_creation
//! 四档 USD/token 单价。cache 按 Anthropic 倍率（cache_read≈0.1×input，
//! cache_creation≈1.25×input）。内置默认表 + config.json 可选覆盖。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// 长上下文整单加价阶梯（对齐 sub2api 对 LiteLLM `*_above_XXXk_tokens` 字段的折算语义）
///
/// 触发条件：`input + cache_read + cache_creation > threshold_tokens`（严格大于，
/// Anthropic 口径）。触发后**整单**重计价：输入侧三段（input/cache_read/cache_creation）
/// 统一乘 `input_multiplier`，输出乘 `output_multiplier`——不是只对超出部分加价。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LongContextLadder {
    pub threshold_tokens: i64,
    pub input_multiplier: f64,
    pub output_multiplier: f64,
}

/// 单个模型的单价（USD / token）
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModelPrice {
    pub input_cost_per_token: f64,
    pub output_cost_per_token: f64,
    pub cache_read_input_token_cost: f64,
    pub cache_creation_input_token_cost: f64,
    /// None = 全窗口平价
    pub long_context: Option<LongContextLadder>,
}

/// config.json 中的单价覆盖项（camelCase）
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelPriceConfig {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_creation: f64,
    /// 长上下文阶梯阈值（token 数，如 200000）。缺省或倍率都 ≤1 时不启用阶梯
    #[serde(default)]
    pub long_context_threshold: Option<i64>,
    /// 长上下文输入侧倍率（input/cache_read/cache_creation 整单同乘）
    #[serde(default)]
    pub long_context_input_multiplier: Option<f64>,
    /// 长上下文输出倍率
    #[serde(default)]
    pub long_context_output_multiplier: Option<f64>,
}

impl ModelPriceConfig {
    /// 折算成阶梯：阈值 >0 且至少一侧倍率 >1 才生效；未配置的一侧按 1 计
    /// （与 sub2api `longContextMultiplierOrOne` 一致，避免乘 0 变免费）
    fn ladder(&self) -> Option<LongContextLadder> {
        let threshold = self.long_context_threshold.unwrap_or(0);
        let in_mul = self.long_context_input_multiplier.unwrap_or(0.0);
        let out_mul = self.long_context_output_multiplier.unwrap_or(0.0);
        let or_one = |m: f64| if m <= 0.0 { 1.0 } else { m };
        if threshold > 0 && (in_mul > 1.0 || out_mul > 1.0) {
            Some(LongContextLadder {
                threshold_tokens: threshold,
                input_multiplier: or_one(in_mul),
                output_multiplier: or_one(out_mul),
            })
        } else {
            None
        }
    }
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
    /// 内置默认表（对齐 sub2api 价格表 model-price-repo，USD/M token，2026-09 同步）
    ///
    /// Opus 4.5+/5：$5/$25；Sonnet 4.x：$3/$15；Sonnet 5：$2/$10（永久介绍价）；
    /// Haiku 4.5：$1/$5。cache_read=0.1×input，cache_creation=1.25×input。
    ///
    /// 长上下文阶梯（与 sub2api 计费一致）：其价格表中仅 claude-sonnet-4 与
    /// claude-sonnet-4-5 带 `above_200k` 档（>200K 整单输入 2×、输出 1.5×）；
    /// claude-sonnet-4-6、Sonnet 5、Opus 全系为全窗口平价（Anthropic 2026-03-13
    /// 起对 4.6+ 取消长上下文加价）。
    pub fn builtin() -> Self {
        // Opus 4.5+ 挂牌价（$5/$25 per M）；cache_read=0.1×input，cache_creation=1.25×input
        let opus = ModelPrice {
            input_cost_per_token: 5.0 / PER_M,
            output_cost_per_token: 25.0 / PER_M,
            cache_read_input_token_cost: 0.5 / PER_M,
            cache_creation_input_token_cost: 6.25 / PER_M,
            long_context: None,
        };
        let sonnet = ModelPrice {
            input_cost_per_token: 3.0 / PER_M,
            output_cost_per_token: 15.0 / PER_M,
            cache_read_input_token_cost: 0.3 / PER_M,
            cache_creation_input_token_cost: 3.75 / PER_M,
            long_context: None,
        };
        // Sonnet 4 / 4.5 长上下文阶梯：above_200k 折算为输入 2×（$6/M）、输出 1.5×（$22.5/M）
        let sonnet_ladder = ModelPrice {
            long_context: Some(LongContextLadder {
                threshold_tokens: 200_000,
                input_multiplier: 2.0,
                output_multiplier: 1.5,
            }),
            ..sonnet
        };
        let haiku = ModelPrice {
            input_cost_per_token: 1.0 / PER_M,
            output_cost_per_token: 5.0 / PER_M,
            cache_read_input_token_cost: 0.1 / PER_M,
            cache_creation_input_token_cost: 1.25 / PER_M,
            long_context: None,
        };
        // Sonnet 5 独立档：$2/$10（上线介绍价，Anthropic 2026-08-10 宣布永久保持，
        // 原定 2026-09-01 涨至 $3/$15 的计划取消）。cache 倍率同标准（0.1× / 1.25×）。
        let sonnet_5 = ModelPrice {
            input_cost_per_token: 2.0 / PER_M,
            output_cost_per_token: 10.0 / PER_M,
            cache_read_input_token_cost: 0.2 / PER_M,
            cache_creation_input_token_cost: 2.5 / PER_M,
            long_context: None,
        };
        let mut table = HashMap::new();
        table.insert("claude-opus-4".to_string(), opus);
        // sonnet-4 前缀档带阶梯（覆盖 sonnet-4 / sonnet-4-5 及带日期后缀变体）；
        // sonnet-4-6 单独插一条平价档，靠最长前缀命中豁免阶梯
        table.insert("claude-sonnet-4".to_string(), sonnet_ladder);
        table.insert("claude-sonnet-4-6".to_string(), sonnet);
        table.insert("claude-haiku-4".to_string(), haiku);
        // Sonnet 5 / Opus 5 显式插入避免依赖 fallback——
        // "claude-sonnet-5" 不以 "claude-sonnet-4" 为前缀，无显式档会静默落 fallback，
        // 后续若上游调价或 fallback 改动会导致计费漂移。
        table.insert("claude-sonnet-5".to_string(), sonnet_5);
        // Opus 5 同理：不以 "claude-opus-4" 为前缀，无显式档会落 sonnet fallback 低估
        table.insert("claude-opus-5".to_string(), opus);
        // 未知模型兜底取 sonnet 平价档（避免低估；不带阶梯，与 sub2api 硬编码
        // fallback 无长上下文字段的行为一致）
        Self { table, fallback: sonnet }
    }

    /// 应用 config 覆盖（按 model 原始 key 精确插入；None 时原样返回）
    ///
    /// 注意：覆盖是整条替换——覆盖内置带阶梯的档（如 claude-sonnet-4-5）时若未
    /// 显式配置 longContext* 字段，该模型即变为全窗口平价（与 sub2api「显式
    /// long_context 配置优先、可用于关闭阶梯」的语义一致）。
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
                        long_context: v.ladder(),
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
            if norm.starts_with(k.as_str()) && best.is_none_or(|(bk, _)| k.len() > bk.len()) {
                best = Some((k, v));
            }
        }
        best.map(|(_, v)| v).unwrap_or(&self.fallback)
    }

    /// 计费纯函数（负数 token 视为 0）
    ///
    /// 长上下文阶梯（对齐 sub2api computeTokenBreakdown）：输入侧三段之和严格大于
    /// 阈值时整单重计价——input/cache_read/cache_creation 同乘输入倍率（缓存本质是
    /// 输入侧复用，跟随输入倍率，见 sub2api #2293），output 乘输出倍率。
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
        let (in_mul, out_mul) = match p.long_context {
            Some(lc)
                if clamp(input) + clamp(cache_read) + clamp(cache_creation)
                    > lc.threshold_tokens as f64 =>
            {
                (lc.input_multiplier, lc.output_multiplier)
            }
            _ => (1.0, 1.0),
        };
        clamp(input) * p.input_cost_per_token * in_mul
            + clamp(cache_read) * p.cache_read_input_token_cost * in_mul
            + clamp(cache_creation) * p.cache_creation_input_token_cost * in_mul
            + clamp(output) * p.output_cost_per_token * out_mul
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builtin_opus_cost_with_cache_multipliers() {
        let t = PricingTable::builtin();
        // opus: input 5/M, output 25/M, cache_read 0.5/M, cache_creation 6.25/M
        // 1M input + 1M output + 1M cache_read + 1M cache_creation
        let cost = t.cost_usd("claude-opus-4-7", 1_000_000, 1_000_000, 1_000_000, 1_000_000);
        // 5 + 25 + 0.5 + 6.25 = 36.75
        assert!((cost - 36.75).abs() < 1e-6, "got {cost}");
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
            ModelPriceConfig {
                input: 0.001,
                output: 0.002,
                cache_read: 0.0001,
                cache_creation: 0.003,
                long_context_threshold: None,
                long_context_input_multiplier: None,
                long_context_output_multiplier: None,
            },
        );
        let t = PricingTable::builtin().with_overrides(Some(&overrides));
        let cost = t.cost_usd("claude-opus-4-7", 1000, 0, 0, 0);
        assert!((cost - 1.0).abs() < 1e-9, "got {cost}"); // 1000 * 0.001
        // 未覆盖的 opus-4-6 仍走内置前缀价
        assert_eq!(t.price_for("claude-opus-4-6"), &ModelPrice {
            input_cost_per_token: 5.0 / 1e6,
            output_cost_per_token: 25.0 / 1e6,
            cache_read_input_token_cost: 0.5 / 1e6,
            cache_creation_input_token_cost: 6.25 / 1e6,
            long_context: None,
        });
    }

    #[test]
    fn test_negative_tokens_clamped() {
        let t = PricingTable::builtin();
        assert_eq!(t.cost_usd("claude-sonnet-4-6", -5, -5, -5, -5), 0.0);
    }

    #[test]
    fn test_sonnet_5_uses_own_tier() {
        let t = PricingTable::builtin();
        // Sonnet 5 独立档 $2/$10(非 Sonnet 4.x 的 $3/$15,Anthropic 2026-08-10 定为永久价)
        let cost = t.cost_usd("claude-sonnet-5", 1_000_000, 0, 0, 1_000_000);
        assert!((cost - (2.0 + 10.0)).abs() < 1e-6, "got {cost}");
        // cache 倍率:read 0.1×($0.2/M),creation 1.25×($2.5/M)
        let cost_cache = t.cost_usd("claude-sonnet-5", 0, 1_000_000, 1_000_000, 0);
        assert!((cost_cache - (0.2 + 2.5)).abs() < 1e-6, "got {cost_cache}");
        // -thinking 变体归一化后同档
        let cost2 = t.cost_usd("claude-sonnet-5-thinking", 1_000_000, 0, 0, 0);
        assert!((cost2 - 2.0).abs() < 1e-6, "got {cost2}");
        // Sonnet 4.6 不受影响,仍是 $3/M
        let cost3 = t.cost_usd("claude-sonnet-4-6", 1_000_000, 0, 0, 0);
        assert!((cost3 - 3.0).abs() < 1e-6, "got {cost3}");
    }

    #[test]
    fn test_opus_5_resolves_to_opus_tier() {
        let t = PricingTable::builtin();
        // claude-opus-5 不以 claude-opus-4 为前缀,需显式档命中 Opus 价($5/$25),
        // 否则静默落 sonnet fallback 低估
        let cost = t.cost_usd("claude-opus-5", 1_000_000, 0, 0, 1_000_000);
        assert!((cost - (5.0 + 25.0)).abs() < 1e-6, "got {cost}");
        // -thinking 变体归一化后同档
        let cost2 = t.cost_usd("claude-opus-5-thinking", 1_000_000, 0, 0, 0);
        assert!((cost2 - 5.0).abs() < 1e-6, "got {cost2}");
    }

    #[test]
    fn test_long_context_ladder_sonnet_4_5_whole_request() {
        let t = PricingTable::builtin();
        // 300K 输入(纯 input) + 1M 输出,超过 200K 阈值 → 整单输入 2×、输出 1.5×
        // input: 0.3M * $3 * 2 = $1.8;output: 1M * $15 * 1.5 = $22.5
        let cost = t.cost_usd("claude-sonnet-4-5", 300_000, 0, 0, 1_000_000);
        assert!((cost - (1.8 + 22.5)).abs() < 1e-6, "got {cost}");
        // 带日期后缀同样命中阶梯
        let cost2 = t.cost_usd("claude-sonnet-4-5-20250929", 300_000, 0, 0, 0);
        assert!((cost2 - 1.8).abs() < 1e-6, "got {cost2}");
    }

    #[test]
    fn test_long_context_threshold_counts_cache_and_is_strict() {
        let t = PricingTable::builtin();
        // 阈值判定包含 cache 三段之和:100K input + 150K cache_read = 250K > 200K
        // → cache_read 也乘输入倍率:0.1M*$3*2 + 0.15M*$0.3*2 = 0.6 + 0.09
        let cost = t.cost_usd("claude-sonnet-4-5", 100_000, 150_000, 0, 0);
        assert!((cost - 0.69).abs() < 1e-6, "got {cost}");
        // 恰好 200K 不触发(严格大于,Anthropic 口径)
        let at = t.cost_usd("claude-sonnet-4-5", 200_000, 0, 0, 0);
        assert!((at - 0.6).abs() < 1e-6, "got {at}");
        // 200K + 1 触发
        let over = t.cost_usd("claude-sonnet-4-5", 200_001, 0, 0, 0);
        assert!(over > 1.2, "got {over}");
    }

    #[test]
    fn test_long_context_flat_models_unaffected() {
        let t = PricingTable::builtin();
        // sub2api 价格表中 sonnet-4-6 / sonnet-5 / opus 全系无 above_200k 档,
        // 大上下文仍平价(Anthropic 2026-03-13 起 4.6+ 全窗口平价)
        let s46 = t.cost_usd("claude-sonnet-4-6", 500_000, 0, 0, 0);
        assert!((s46 - 1.5).abs() < 1e-6, "got {s46}"); // 0.5M * $3
        let s5 = t.cost_usd("claude-sonnet-5", 500_000, 0, 0, 0);
        assert!((s5 - 1.0).abs() < 1e-6, "got {s5}"); // 0.5M * $2
        let o5 = t.cost_usd("claude-opus-5", 500_000, 0, 0, 0);
        assert!((o5 - 2.5).abs() < 1e-6, "got {o5}"); // 0.5M * $5
        // fallback 档也不带阶梯
        let unknown = t.cost_usd("some-unknown-model", 500_000, 0, 0, 0);
        assert!((unknown - 1.5).abs() < 1e-6, "got {unknown}");
    }

    #[test]
    fn test_long_context_ladder_via_config_override() {
        let mut overrides = HashMap::new();
        // 覆盖 sonnet-4-6 并给它配一个阶梯(验证 config 可开)
        overrides.insert(
            "claude-sonnet-4-6".to_string(),
            ModelPriceConfig {
                input: 3e-6,
                output: 15e-6,
                cache_read: 0.3e-6,
                cache_creation: 3.75e-6,
                long_context_threshold: Some(100_000),
                long_context_input_multiplier: Some(2.0),
                long_context_output_multiplier: None, // 未配置一侧按 1 计,不是 0
            },
        );
        let t = PricingTable::builtin().with_overrides(Some(&overrides));
        let cost = t.cost_usd("claude-sonnet-4-6", 150_000, 0, 0, 100_000);
        // input 0.15M*$3*2 = 0.9;output 0.1M*$15*1 = 1.5(输出倍率缺省为 1)
        assert!((cost - (0.9 + 1.5)).abs() < 1e-6, "got {cost}");
        // 覆盖内置带阶梯的档但不配 longContext* → 阶梯被关闭
        let mut off = HashMap::new();
        off.insert(
            "claude-sonnet-4-5".to_string(),
            ModelPriceConfig {
                input: 3e-6,
                output: 15e-6,
                cache_read: 0.3e-6,
                cache_creation: 3.75e-6,
                long_context_threshold: None,
                long_context_input_multiplier: None,
                long_context_output_multiplier: None,
            },
        );
        let t2 = PricingTable::builtin().with_overrides(Some(&off));
        let flat = t2.cost_usd("claude-sonnet-4-5", 500_000, 0, 0, 0);
        assert!((flat - 1.5).abs() < 1e-6, "got {flat}");
    }

    #[test]
    fn test_haiku_cost_resolves() {
        let t = PricingTable::builtin();
        // haiku: input 1/M → 1M input = $1 (NOT sonnet's $3)
        let cost = t.cost_usd("claude-haiku-4-5", 1_000_000, 0, 0, 0);
        assert!((cost - 1.0).abs() < 1e-6, "got {cost}");
        // 带日期后缀的完整 id 也应命中 haiku 档
        let cost2 = t.cost_usd("claude-haiku-4-5-20251001", 1_000_000, 0, 0, 0);
        assert!((cost2 - 1.0).abs() < 1e-6, "got {cost2}");
    }
}
