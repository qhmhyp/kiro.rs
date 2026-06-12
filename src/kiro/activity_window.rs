//! 每凭据滚动活动窗口(最近 15 分钟)
//!
//! 为限速观测提供"事发时刻负载"统计;条数上限兜底防止异常流量撑爆内存。
//! `last_429_at` 不随 15 分钟窗口修剪——"距上次 429 多久"本身就是跨窗口的信号,
//! 分析侧可自行过滤过大的值。

use chrono::{DateTime, Duration, Utc};
use std::collections::VecDeque;

/// 窗口长度:15 分钟(覆盖 jq 分析所需的 1m/5m 统计并留余量)
fn window() -> Duration {
    Duration::minutes(15)
}

/// 单窗口事件条数上限(15 分钟内超过即丢最旧,纯内存兜底)
const MAX_EVENTS: usize = 2048;

#[derive(Default)]
pub struct ActivityWindow {
    starts: VecDeque<DateTime<Utc>>,
    token_events: VecDeque<TokenEvent>,
    last_429_at: Option<DateTime<Utc>>,
}

struct TokenEvent {
    at: DateTime<Utc>,
    input: u64,
    output: u64,
}

/// `ActivityWindow::stats` 的导出结果
#[derive(Debug, Clone, Copy, Default)]
pub struct WindowStats {
    pub req_1m: u32,
    pub req_5m: u32,
    pub tokens_in_1m: u64,
    pub tokens_out_1m: u64,
    pub secs_since_last_429: Option<i64>,
}

impl ActivityWindow {
    pub fn record_start(&mut self, now: DateTime<Utc>) {
        debug_assert!(
            self.starts.back().map_or(true, |last| *last <= now),
            "record_start 的 now 不应早于上一次记录"
        );
        self.prune(now);
        self.starts.push_back(now);
        if self.starts.len() > MAX_EVENTS {
            self.starts.pop_front();
        }
    }

    pub fn record_tokens(&mut self, now: DateTime<Utc>, input: u64, output: u64) {
        debug_assert!(
            self.token_events.back().map_or(true, |last| last.at <= now),
            "record_tokens 的 now 不应早于上一次记录"
        );
        self.prune(now);
        self.token_events.push_back(TokenEvent { at: now, input, output });
        if self.token_events.len() > MAX_EVENTS {
            self.token_events.pop_front();
        }
    }

    pub fn record_429(&mut self, now: DateTime<Utc>) {
        self.last_429_at = Some(now);
    }

    pub fn stats(&mut self, now: DateTime<Utc>) -> WindowStats {
        self.prune(now);
        let m1 = now - Duration::seconds(60);
        let m5 = now - Duration::seconds(300);
        WindowStats {
            req_1m: self.starts.iter().filter(|t| **t >= m1).count() as u32,
            req_5m: self.starts.iter().filter(|t| **t >= m5).count() as u32,
            tokens_in_1m: self
                .token_events
                .iter()
                .filter(|e| e.at >= m1)
                .map(|e| e.input)
                .sum(),
            tokens_out_1m: self
                .token_events
                .iter()
                .filter(|e| e.at >= m1)
                .map(|e| e.output)
                .sum(),
            secs_since_last_429: self.last_429_at.map(|t| (now - t).num_seconds()),
        }
    }

    fn prune(&mut self, now: DateTime<Utc>) {
        // 前提:record_* 按单调递增时间调用(队列从旧到新)。
        // now 倒退时 prune 提前停止——静默降级,不 panic,观测数据短暂偏高可接受。
        let cutoff = now - window();
        while self.starts.front().is_some_and(|t| *t < cutoff) {
            self.starts.pop_front();
        }
        while self.token_events.front().is_some_and(|e| e.at < cutoff) {
            self.token_events.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs_ago: i64) -> DateTime<Utc> {
        Utc::now() - Duration::seconds(secs_ago)
    }

    #[test]
    fn counts_requests_in_1m_and_5m() {
        let mut w = ActivityWindow::default();
        w.record_start(at(200)); // 仅计入 5m
        w.record_start(at(30)); // 计入 1m + 5m
        w.record_start(at(5));
        let s = w.stats(Utc::now());
        assert_eq!(s.req_1m, 2);
        assert_eq!(s.req_5m, 3);
    }

    #[test]
    fn sums_tokens_in_1m_only() {
        let mut w = ActivityWindow::default();
        w.record_tokens(at(120), 1000, 50); // 超出 1m,不计
        w.record_tokens(at(10), 200, 30);
        w.record_tokens(at(5), 300, 20);
        let s = w.stats(Utc::now());
        assert_eq!(s.tokens_in_1m, 500);
        assert_eq!(s.tokens_out_1m, 50);
    }

    #[test]
    fn prunes_events_older_than_window() {
        let mut w = ActivityWindow::default();
        w.record_start(at(16 * 60)); // 超出 15 分钟窗口
        w.record_start(at(10));
        let s = w.stats(Utc::now());
        assert_eq!(s.req_5m, 1);
        assert_eq!(w.starts.len(), 1, "过期事件应被修剪释放内存");
    }

    #[test]
    fn caps_event_count() {
        let mut w = ActivityWindow::default();
        for _ in 0..(MAX_EVENTS + 100) {
            w.record_start(at(1));
        }
        assert!(w.starts.len() <= MAX_EVENTS);
    }

    #[test]
    fn tracks_secs_since_last_429() {
        let mut w = ActivityWindow::default();
        assert_eq!(w.stats(Utc::now()).secs_since_last_429, None);
        w.record_429(at(90));
        let s = w.stats(Utc::now());
        assert!((89..=92).contains(&s.secs_since_last_429.unwrap()));
    }
}
