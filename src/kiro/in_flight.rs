//! 凭据级在途请求计数
//!
//! RAII guard 保证任何退出路径(early return / 客户端断流 / panic)都归还计数,
//! 避免配对式 +1/-1 漏减导致计数漂移。

use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

#[derive(Default)]
pub struct InFlightTracker {
    /// Drop 路径(计数归还)不加锁,直接走 Arc 内原子量;仅插入新凭据及 get/snapshot 读取时短暂持锁
    counters: Mutex<HashMap<u64, Arc<InFlightCounter>>>,
}

#[derive(Default)]
struct InFlightCounter {
    current: AtomicU32,
    /// 进程启动以来最高瞬时并发,不持久化
    peak: AtomicU32,
}

/// Drop 时自动归还计数
#[must_use = "guard 必须存活到请求结束,立即丢弃会让计数瞬间归零"]
pub struct InFlightGuard {
    counter: Arc<InFlightCounter>,
}

impl InFlightTracker {
    /// 登记一次在途请求,返回的 guard drop 时自动归还
    pub fn track(&self, id: u64) -> InFlightGuard {
        let counter = {
            let mut map = self.counters.lock();
            map.entry(id).or_default().clone()
        };
        let cur = counter.current.fetch_add(1, Ordering::AcqRel) + 1;
        counter.peak.fetch_max(cur, Ordering::AcqRel);
        InFlightGuard { counter }
    }

    /// (当前并发, 历史峰值);未知凭据返回 (0, 0)
    pub fn get(&self, id: u64) -> (u32, u32) {
        self.counters
            .lock()
            .get(&id)
            .map(|c| {
                (
                    c.current.load(Ordering::Acquire),
                    c.peak.load(Ordering::Acquire),
                )
            })
            .unwrap_or((0, 0))
    }

    /// 全量快照:id → (当前并发, 历史峰值)
    pub fn snapshot(&self) -> HashMap<u64, (u32, u32)> {
        self.counters
            .lock()
            .iter()
            .map(|(id, c)| {
                (
                    *id,
                    (
                        c.current.load(Ordering::Acquire),
                        c.peak.load(Ordering::Acquire),
                    ),
                )
            })
            .collect()
    }
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.counter.current.fetch_sub(1, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_increments_and_drop_decrements() {
        let t = InFlightTracker::default();
        let g1 = t.track(1);
        let g2 = t.track(1);
        assert_eq!(t.get(1), (2, 2));
        drop(g1);
        assert_eq!(t.get(1), (1, 2), "drop 归还 current,peak 保留");
        drop(g2);
        assert_eq!(t.get(1), (0, 2));
    }

    #[test]
    fn unknown_credential_reads_zero() {
        let t = InFlightTracker::default();
        assert_eq!(t.get(99), (0, 0));
    }

    #[test]
    fn peak_tracks_concurrent_max() {
        let t = InFlightTracker::default();
        let guards: Vec<_> = (0..5).map(|_| t.track(7)).collect();
        assert_eq!(t.get(7), (5, 5));
        drop(guards);
        let _g = t.track(7);
        assert_eq!(t.get(7), (1, 5), "峰值不随回落下降");
    }

    #[test]
    fn snapshot_returns_all_tracked_ids() {
        let t = InFlightTracker::default();
        let _g1 = t.track(1);
        let _g2 = t.track(2);
        let snap = t.snapshot();
        assert_eq!(snap.get(&1), Some(&(1, 1)));
        assert_eq!(snap.get(&2), Some(&(1, 1)));
    }

    #[test]
    fn guard_returns_count_when_stream_dropped() {
        // 钉住流式接线 idiom:guard 被 move 进 stream.map 闭包,流 drop 时归还
        use futures::StreamExt;
        let t = InFlightTracker::default();
        let guard = t.track(1);
        let s = futures::stream::iter(vec![1, 2, 3]);
        let s = s.map(move |x| {
            let _keep = &guard;
            x
        });
        assert_eq!(t.get(1).0, 1);
        drop(s); // 模拟客户端中途断开
        assert_eq!(t.get(1).0, 0, "流被 drop 后计数必须归还");
    }
}
