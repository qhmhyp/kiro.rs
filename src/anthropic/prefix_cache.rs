//! 会话级 usage 缓存模拟
//!
//! 这个模块本身**不缓存任何上游响应、不省任何上游调用**，纯粹是为了在返回给客户端的
//! `usage` 字段里把 `input_tokens` 拆分成 `cache_read_input_tokens` /
//! `cache_creation_input_tokens` / `input_tokens` 三段，让下游计费工具能像识别真正的
//! Anthropic prompt cache 一样识别"缓存命中"。
//!
//! # 拆分规则
//! 给定本轮真实的总输入 token 数 `total_input_tokens` 和当前 user 消息的 token 数
//! `current_turn_tokens`：
//!
//! - `history_tokens = total_input_tokens - current_turn_tokens`（即 system + 历史）
//! - 若同一 `conversation_id` 上一轮记录的 history 大小为 `prev`：
//!   - `cache_read = min(prev, history_tokens)`
//!   - `cache_creation = history_tokens - cache_read`
//!   - `input_tokens = current_turn_tokens`
//! - 三者之和恒等于 `total_input_tokens`，下游成本归因不会出现"对不上"的情况。
//!
//! # TTL
//! 默认使用 moka 的 `time_to_idle(5min)`，对齐 Anthropic ephemeral cache 语义：
//! 用户连续聊天时一直命中，停顿 5 分钟后再来才回到首轮"建立缓存"形态。
//! 空闲时长可经 config.json `usageCacheIdleSecs` 调整（0 = 永不过期）；
//! `usageCacheEnabled=false` 时整个模拟关闭，全部输入按 `input_tokens` 全价上报。

use moka::sync::Cache;
use parking_lot::RwLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

/// 缓存条目最大数量。50K 个并发会话约占 5 MB 内存。
const CACHE_CAPACITY: u64 = 50_000;

/// 会话空闲过期时间默认值。对齐 Anthropic ephemeral cache 默认值。
const CACHE_IDLE_SECS: u64 = 300;

/// usage 模拟缓存当前生效的参数快照
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UsageCacheSettings {
    pub enabled: bool,
    pub idle_secs: u64,
    pub read_ratio: f64,
}

/// 会话级 token 缓存
///
/// key = conversation_id（来自 metadata.user_id 中提取的 session UUID，
/// 或 convert_request 在缺省时分配的 UUID）
/// value = 上一次该会话请求中"历史部分"的 token 数
///
/// 三个参数均支持运行时热更新（Admin API），因此用原子量/RwLock 存储。
/// moka 的 time_to_idle 在构建时固化，修改 idle_secs 时重建内层缓存
/// （已有会话的命中状态清零，各会话下一轮回到"重建缓存"形态，可接受）。
pub struct ConvoTokenCache {
    inner: RwLock<Cache<String, u32>>,
    /// false 时关闭模拟：peek 返回 (0, 0, total)，commit 为 no-op
    enabled: AtomicBool,
    idle_secs: AtomicU64,
    /// 命中部分按该比例上报为 cache_read（0.1× 计费），其余滑回 input_tokens（1× 计费）。
    /// 以 f64 的 bit 表示存储，写入前已被夹到 [0.0, 1.0]。
    read_ratio_bits: AtomicU64,
}

impl Default for ConvoTokenCache {
    fn default() -> Self {
        Self::new()
    }
}

/// 归一化 read_ratio：NaN 兜底到 1.0（保持现状行为），其余夹到 [0, 1]
fn normalize_ratio(ratio: f64) -> f64 {
    if ratio.is_nan() { 1.0 } else { ratio.clamp(0.0, 1.0) }
}

/// 构建内层 moka 缓存（idle_secs=0 表示永不过期）
fn build_inner(idle_secs: u64) -> Cache<String, u32> {
    let mut builder = Cache::builder().max_capacity(CACHE_CAPACITY);
    if idle_secs > 0 {
        builder = builder.time_to_idle(Duration::from_secs(idle_secs));
    }
    builder.build()
}

impl ConvoTokenCache {
    pub fn new() -> Self {
        Self::with_options(true, CACHE_IDLE_SECS, 1.0)
    }

    /// 按配置构建
    ///
    /// - `enabled=false`：关闭 usage 缓存模拟，所有输入按 `input_tokens` 全价上报
    ///   （下游计费不再有 cache_read 0.1× 折扣，长对话下计费显著提高）
    /// - `idle_secs=0`：永不过期，会话只要仍在容量内就持续命中
    /// - 其他值：会话空闲超过该秒数后，下一轮回到"重建缓存"形态
    /// - `read_ratio`：命中折扣比例，1.0 = 全额 cache_read（现状），0.0 = 命中部分
    ///   全部按 input_tokens 全价上报；越小下游计费越高。超出 [0,1] 自动夹取
    pub fn with_options(enabled: bool, idle_secs: u64, read_ratio: f64) -> Self {
        Self {
            inner: RwLock::new(build_inner(idle_secs)),
            enabled: AtomicBool::new(enabled),
            idle_secs: AtomicU64::new(idle_secs),
            read_ratio_bits: AtomicU64::new(normalize_ratio(read_ratio).to_bits()),
        }
    }

    /// 当前生效的参数快照
    pub fn settings(&self) -> UsageCacheSettings {
        UsageCacheSettings {
            enabled: self.enabled.load(Ordering::Relaxed),
            idle_secs: self.idle_secs.load(Ordering::Relaxed),
            read_ratio: f64::from_bits(self.read_ratio_bits.load(Ordering::Relaxed)),
        }
    }

    /// 运行时更新参数（Admin API 热更新入口）
    ///
    /// idle_secs 变化时重建内层缓存：已有会话的命中状态清零，
    /// 各会话下一轮按"重建缓存"形态上报（cache_creation 1.25×）。
    pub fn apply_settings(&self, settings: UsageCacheSettings) {
        self.enabled.store(settings.enabled, Ordering::Relaxed);
        self.read_ratio_bits
            .store(normalize_ratio(settings.read_ratio).to_bits(), Ordering::Relaxed);

        let old_idle = self.idle_secs.swap(settings.idle_secs, Ordering::Relaxed);
        if old_idle != settings.idle_secs {
            *self.inner.write() = build_inner(settings.idle_secs);
        }
    }

    /// 计算 (cache_read, cache_creation, input_tokens) 三元组（不写入缓存）
    ///
    /// 同一个请求里可以多次调用：流式响应 message_start 阶段用估算的 total
    /// 调一次，message_delta 阶段用 contextUsageEvent 反推的真实 total 再调一次。
    pub fn peek(
        &self,
        conversation_id: &str,
        total_input_tokens: i32,
        current_turn_tokens: i32,
    ) -> (i32, i32, i32) {
        let total = total_input_tokens.max(0);
        if !self.enabled.load(Ordering::Relaxed) {
            // 模拟关闭：不拆分，全部按 input_tokens 上报（三段之和仍等于 total）
            return (0, 0, total);
        }
        let current_turn = current_turn_tokens.clamp(0, total);
        let history = total - current_turn;

        let prev = self.inner.read().get(conversation_id).unwrap_or(0) as i32;
        // eligible = 本可全额命中的部分；read_ratio < 1.0 时只按比例上报为
        // cache_read，差额滑回 input_tokens（全价），cache_creation 不受影响。
        // 三段之和仍恒等于 total。
        let read_ratio = f64::from_bits(self.read_ratio_bits.load(Ordering::Relaxed));
        let eligible = prev.min(history);
        let cache_read = (eligible as f64 * read_ratio).floor() as i32;
        let cache_creation = history - eligible;
        let input = current_turn + (eligible - cache_read);

        (cache_read, cache_creation, input)
    }

    /// 提交本轮的 history 大小，供下一轮 peek 使用
    ///
    /// 每个请求只应调用一次，时机：
    /// - 非流式：构建完最终 usage 之后
    /// - 流式：generate_final_events 时（此时 contextUsageEvent 的真实值已知）
    pub fn commit(
        &self,
        conversation_id: &str,
        total_input_tokens: i32,
        current_turn_tokens: i32,
    ) {
        if !self.enabled.load(Ordering::Relaxed) {
            return;
        }
        let total = total_input_tokens.max(0);
        let current_turn = current_turn_tokens.clamp(0, total);
        let history = (total - current_turn).max(0) as u32;
        self.inner.read().insert(conversation_id.to_string(), history);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_turn_all_creation() {
        let cache = ConvoTokenCache::new();
        let (read, creation, input) = cache.peek("conv-1", 10_000, 200);
        assert_eq!(read, 0);
        assert_eq!(creation, 9_800);
        assert_eq!(input, 200);
        assert_eq!(read + creation + input, 10_000);
    }

    #[test]
    fn second_turn_full_hit() {
        let cache = ConvoTokenCache::new();
        cache.commit("conv-1", 10_000, 200); // 上轮: history = 9_800
        let (read, creation, input) = cache.peek("conv-1", 12_000, 300);
        // 本轮 history = 11_700, prev = 9_800
        assert_eq!(read, 9_800);
        assert_eq!(creation, 1_900); // 新增 1_900 token 历史
        assert_eq!(input, 300);
        assert_eq!(read + creation + input, 12_000);
    }

    #[test]
    fn shrinking_history_caps_at_actual() {
        let cache = ConvoTokenCache::new();
        cache.commit("conv-1", 20_000, 200); // 上轮 history = 19_800
        let (read, creation, input) = cache.peek("conv-1", 5_000, 100);
        // 本轮 history 只有 4_900，cache_read 不能超过它
        assert_eq!(read, 4_900);
        assert_eq!(creation, 0);
        assert_eq!(input, 100);
        assert_eq!(read + creation + input, 5_000);
    }

    #[test]
    fn unknown_conversation_no_hit() {
        let cache = ConvoTokenCache::new();
        let (read, creation, input) = cache.peek("never-seen", 5_000, 100);
        assert_eq!(read, 0);
        assert_eq!(creation, 4_900);
        assert_eq!(input, 100);
    }

    #[test]
    fn disabled_reports_all_as_input() {
        let cache = ConvoTokenCache::with_options(false, 300, 1.0);
        // commit 是 no-op，peek 永远不拆分
        cache.commit("conv-1", 10_000, 200);
        let (read, creation, input) = cache.peek("conv-1", 12_000, 300);
        assert_eq!(read, 0);
        assert_eq!(creation, 0);
        assert_eq!(input, 12_000);
    }

    #[test]
    fn disabled_still_clamps_negative_total() {
        let cache = ConvoTokenCache::with_options(false, 300, 1.0);
        let (read, creation, input) = cache.peek("conv-1", -5, 100);
        assert_eq!((read, creation, input), (0, 0, 0));
    }

    #[test]
    fn zero_idle_secs_means_no_expiry() {
        // idle_secs=0 构建时不设置 time_to_idle,行为与正常缓存一致(逻辑层面)
        let cache = ConvoTokenCache::with_options(true, 0, 1.0);
        cache.commit("conv-1", 10_000, 200);
        let (read, creation, input) = cache.peek("conv-1", 12_000, 300);
        assert_eq!(read, 9_800);
        assert_eq!(creation, 1_900);
        assert_eq!(input, 300);
    }

    #[test]
    fn custom_idle_secs_builds_and_works() {
        let cache = ConvoTokenCache::with_options(true, 60, 1.0);
        cache.commit("conv-1", 10_000, 200);
        let (read, _, _) = cache.peek("conv-1", 12_000, 300);
        assert_eq!(read, 9_800);
    }

    #[test]
    fn read_ratio_half_shifts_rest_to_input() {
        let cache = ConvoTokenCache::with_options(true, 300, 0.5);
        cache.commit("conv-1", 10_000, 200); // prev history = 9_800
        let (read, creation, input) = cache.peek("conv-1", 12_000, 300);
        // eligible = 9_800，按 0.5 上报 4_900 为 cache_read，其余 4_900 滑回 input
        assert_eq!(read, 4_900);
        assert_eq!(creation, 1_900); // 新增历史部分不受 ratio 影响
        assert_eq!(input, 300 + 4_900);
        assert_eq!(read + creation + input, 12_000); // 总和不变量
    }

    #[test]
    fn read_ratio_zero_no_discount_but_still_tracks() {
        let cache = ConvoTokenCache::with_options(true, 300, 0.0);
        cache.commit("conv-1", 10_000, 200);
        let (read, creation, input) = cache.peek("conv-1", 12_000, 300);
        assert_eq!(read, 0);
        assert_eq!(creation, 1_900);
        assert_eq!(input, 300 + 9_800);
        assert_eq!(read + creation + input, 12_000);
    }

    #[test]
    fn read_ratio_out_of_range_clamped() {
        // >1 夹到 1.0(全额命中)
        let cache = ConvoTokenCache::with_options(true, 300, 1.5);
        cache.commit("conv-1", 10_000, 200);
        let (read, _, _) = cache.peek("conv-1", 12_000, 300);
        assert_eq!(read, 9_800);
        // <0 夹到 0.0(无折扣)
        let cache = ConvoTokenCache::with_options(true, 300, -0.5);
        cache.commit("conv-1", 10_000, 200);
        let (read, _, input) = cache.peek("conv-1", 12_000, 300);
        assert_eq!(read, 0);
        assert_eq!(input, 300 + 9_800);
        // NaN 兜底到 1.0(保持现状)
        let cache = ConvoTokenCache::with_options(true, 300, f64::NAN);
        cache.commit("conv-1", 10_000, 200);
        let (read, _, _) = cache.peek("conv-1", 12_000, 300);
        assert_eq!(read, 9_800);
    }

    #[test]
    fn apply_settings_hot_update() {
        let cache = ConvoTokenCache::new();
        cache.commit("conv-1", 10_000, 200); // history = 9_800

        // 运行时关闭模拟
        cache.apply_settings(UsageCacheSettings { enabled: false, idle_secs: 300, read_ratio: 1.0 });
        assert_eq!(cache.peek("conv-1", 12_000, 300), (0, 0, 12_000));

        // 重新开启 + 调 ratio；idle 不变时已有条目保留
        cache.apply_settings(UsageCacheSettings { enabled: true, idle_secs: 300, read_ratio: 0.5 });
        let (read, _, input) = cache.peek("conv-1", 12_000, 300);
        assert_eq!(read, 4_900);
        assert_eq!(input, 300 + 4_900);

        // 修改 idle_secs 触发内层重建，已有条目清零
        cache.apply_settings(UsageCacheSettings { enabled: true, idle_secs: 60, read_ratio: 1.0 });
        let (read, creation, input) = cache.peek("conv-1", 12_000, 300);
        assert_eq!((read, creation, input), (0, 11_700, 300));
        assert_eq!(cache.settings().idle_secs, 60);

        // settings() 快照反映归一化后的值
        cache.apply_settings(UsageCacheSettings { enabled: true, idle_secs: 60, read_ratio: 2.0 });
        assert_eq!(cache.settings().read_ratio, 1.0);
    }

    #[test]
    fn current_turn_clamped_to_total() {
        let cache = ConvoTokenCache::new();
        let (read, creation, input) = cache.peek("conv-x", 100, 999_999);
        // 异常情况下 current_turn 被夹到 total
        assert_eq!(read, 0);
        assert_eq!(creation, 0);
        assert_eq!(input, 100);
    }
}
