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
//! 使用 moka 的 `time_to_idle(5min)`，对齐 Anthropic ephemeral cache 语义：
//! 用户连续聊天时一直命中，停顿 5 分钟后再来才回到首轮"建立缓存"形态。

use moka::sync::Cache;
use std::time::Duration;

/// 缓存条目最大数量。50K 个并发会话约占 5 MB 内存。
const CACHE_CAPACITY: u64 = 50_000;

/// 会话空闲过期时间。对齐 Anthropic ephemeral cache 默认值。
const CACHE_IDLE_SECS: u64 = 300;

/// 会话级 token 缓存
///
/// key = conversation_id（来自 metadata.user_id 中提取的 session UUID，
/// 或 convert_request 在缺省时分配的 UUID）
/// value = 上一次该会话请求中"历史部分"的 token 数
#[derive(Clone)]
pub struct ConvoTokenCache {
    inner: Cache<String, u32>,
}

impl Default for ConvoTokenCache {
    fn default() -> Self {
        Self::new()
    }
}

impl ConvoTokenCache {
    pub fn new() -> Self {
        Self {
            inner: Cache::builder()
                .max_capacity(CACHE_CAPACITY)
                .time_to_idle(Duration::from_secs(CACHE_IDLE_SECS))
                .build(),
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
        let current_turn = current_turn_tokens.clamp(0, total);
        let history = total - current_turn;

        let prev = self.inner.get(conversation_id).unwrap_or(0) as i32;
        let cache_read = prev.min(history);
        let cache_creation = history - cache_read;

        (cache_read, cache_creation, current_turn)
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
        let total = total_input_tokens.max(0);
        let current_turn = current_turn_tokens.clamp(0, total);
        let history = (total - current_turn).max(0) as u32;
        self.inner.insert(conversation_id.to_string(), history);
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
    fn current_turn_clamped_to_total() {
        let cache = ConvoTokenCache::new();
        let (read, creation, input) = cache.peek("conv-x", 100, 999_999);
        // 异常情况下 current_turn 被夹到 total
        assert_eq!(read, 0);
        assert_eq!(creation, 0);
        assert_eq!(input, 100);
    }
}
