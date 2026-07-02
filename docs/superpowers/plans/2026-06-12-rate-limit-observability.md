# 凭据限速观测(Rate Limit Observability)实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 记录每凭据限速事件(429/402/403)发生时刻的负载现场与日常负载基线,用于离线分析 Kiro 限速触发条件;同时在 admin UI 实时展示每凭据并发数。

**Architecture:** 三个独立小模块(在途计数 `in_flight.rs`、滚动窗口 `activity_window.rs`、JSONL 落盘 `incident_log.rs`)挂到 `MultiTokenManager` 上;provider 重试循环为每次上游尝试创建 RAII guard 并在 429/402/40x 分支打事故快照;后台任务每 60 秒写基线样本;admin API/UI 透出并发字段。

**Tech Stack:** Rust (axum/tokio/parking_lot/chrono/serde,均为现有依赖,**不新增依赖**);admin-ui 为 React + TanStack Query。

**Spec:** `docs/superpowers/specs/2026-06-12-rate-limit-observability-design.md`

**与 spec 的一处偏差(已论证):** spec 写"guard 放进 `CallContext`",但 `CallContext` 是 `#[derive(Clone)]`(`token_manager.rs:617`),guard 入字段会带来克隆双计问题。实现改为:在 `call_api_with_retry` 等调用方 acquire 成功后**独立创建 guard**,覆盖语义完全相同。

**代码注释规范:** 本仓库注释为中文、解释"为什么"而非"做什么",新代码保持一致。

---

### Task 1: InFlightTracker(在途计数 + RAII guard)

**Files:**
- Create: `src/kiro/in_flight.rs`
- Modify: `src/kiro/mod.rs`(加 `pub mod in_flight;`)

- [ ] **Step 1.1: 注册模块并写失败测试**

在 `src/kiro/mod.rs` 的 `pub mod endpoint;` 之前加一行:

```rust
pub mod in_flight;
```

创建 `src/kiro/in_flight.rs`,先只写测试和空骨架:

```rust
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
    /// 仅插入新凭据时短暂持锁;计数增减走 Arc 内原子量,不经过此锁
    counters: Mutex<HashMap<u64, Arc<InFlightCounter>>>,
}

#[derive(Default)]
struct InFlightCounter {
    current: AtomicU32,
    /// 进程启动以来最高瞬时并发,不持久化
    peak: AtomicU32,
}

/// Drop 时自动归还计数
pub struct InFlightGuard {
    counter: Arc<InFlightCounter>,
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
```

- [ ] **Step 1.2: 运行测试确认编译失败**

Run: `cargo test --lib kiro::in_flight 2>&1 | tail -5`
Expected: 编译错误,`track`/`get`/`snapshot` 方法不存在。

- [ ] **Step 1.3: 实现最小代码**

在 `in_flight.rs` 的 `InFlightGuard` 定义后补实现:

```rust
impl InFlightTracker {
    /// 登记一次在途请求,返回的 guard drop 时自动归还
    pub fn track(&self, id: u64) -> InFlightGuard {
        let counter = {
            let mut map = self.counters.lock();
            map.entry(id).or_default().clone()
        };
        let cur = counter.current.fetch_add(1, Ordering::SeqCst) + 1;
        counter.peak.fetch_max(cur, Ordering::SeqCst);
        InFlightGuard { counter }
    }

    /// (当前并发, 历史峰值);未知凭据返回 (0, 0)
    pub fn get(&self, id: u64) -> (u32, u32) {
        self.counters
            .lock()
            .get(&id)
            .map(|c| {
                (
                    c.current.load(Ordering::SeqCst),
                    c.peak.load(Ordering::SeqCst),
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
                        c.current.load(Ordering::SeqCst),
                        c.peak.load(Ordering::SeqCst),
                    ),
                )
            })
            .collect()
    }
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.counter.current.fetch_sub(1, Ordering::SeqCst);
    }
}
```

- [ ] **Step 1.4: 运行测试确认通过**

Run: `cargo test --lib kiro::in_flight 2>&1 | tail -5`
Expected: `5 passed`

- [ ] **Step 1.5: Commit**

```bash
git add src/kiro/mod.rs src/kiro/in_flight.rs
git commit -m "feat(observability): 凭据级在途计数 InFlightTracker(RAII guard)"
```

---

### Task 2: ActivityWindow(滚动活动窗口)

**Files:**
- Create: `src/kiro/activity_window.rs`
- Modify: `src/kiro/mod.rs`(加 `pub mod activity_window;`)

- [ ] **Step 2.1: 注册模块并写失败测试**

`src/kiro/mod.rs` 加 `pub mod activity_window;`。创建 `src/kiro/activity_window.rs`:

```rust
//! 每凭据滚动活动窗口(最近 15 分钟)
//!
//! 为限速观测提供"事发时刻负载"统计;条数上限兜底防止异常流量撑爆内存。

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
```

- [ ] **Step 2.2: 运行测试确认编译失败**

Run: `cargo test --lib kiro::activity_window 2>&1 | tail -5`
Expected: 编译错误,方法不存在。

- [ ] **Step 2.3: 实现最小代码**

```rust
impl ActivityWindow {
    pub fn record_start(&mut self, now: DateTime<Utc>) {
        self.prune(now);
        self.starts.push_back(now);
        if self.starts.len() > MAX_EVENTS {
            self.starts.pop_front();
        }
    }

    pub fn record_tokens(&mut self, now: DateTime<Utc>, input: u64, output: u64) {
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
        let cutoff = now - window();
        while self.starts.front().is_some_and(|t| *t < cutoff) {
            self.starts.pop_front();
        }
        while self.token_events.front().is_some_and(|e| e.at < cutoff) {
            self.token_events.pop_front();
        }
    }
}
```

- [ ] **Step 2.4: 运行测试确认通过**

Run: `cargo test --lib kiro::activity_window 2>&1 | tail -5`
Expected: `5 passed`

- [ ] **Step 2.5: Commit**

```bash
git add src/kiro/mod.rs src/kiro/activity_window.rs
git commit -m "feat(observability): 每凭据滚动活动窗口 ActivityWindow"
```

---

### Task 3: IncidentRecord + JSONL 落盘

**Files:**
- Create: `src/kiro/incident_log.rs`
- Modify: `src/kiro/mod.rs`(加 `pub mod incident_log;`)

- [ ] **Step 3.1: 注册模块并写失败测试**

`src/kiro/mod.rs` 加 `pub mod incident_log;`。创建 `src/kiro/incident_log.rs`:

```rust
//! 限速观测记录的 JSONL 落盘
//!
//! best-effort:写失败只打 debug 日志,绝不影响代理主流程。
//! 落盘原因:docker 日志会滚动,JSONL 才能攒几天数据离线分析。

use serde::Serialize;
use std::io::Write;
use std::path::Path;

/// 事故快照 / 基线采样共用一条记录结构,靠 `kind` 区分:
/// `429_transient | 429_suspicious | 402_quota | 40x_auth | baseline`
#[derive(Debug, Serialize)]
pub struct IncidentRecord {
    /// RFC3339 时间戳
    pub ts: String,
    pub credential: u64,
    pub kind: String,
    pub in_flight: u32,
    pub in_flight_peak: u32,
    pub req_1m: u32,
    pub req_5m: u32,
    pub tokens_in_1m: u64,
    pub tokens_out_1m: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secs_since_last_429: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cooldown_secs: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> IncidentRecord {
        IncidentRecord {
            ts: "2026-06-12T00:00:00Z".into(),
            credential: 22,
            kind: "429_transient".into(),
            in_flight: 3,
            in_flight_peak: 7,
            req_1m: 12,
            req_5m: 41,
            tokens_in_1m: 85000,
            tokens_out_1m: 12000,
            model: Some("claude-sonnet-4-5".into()),
            attempt: Some(2),
            secs_since_last_429: Some(183),
            cooldown_secs: Some(60),
        }
    }

    #[test]
    fn appends_one_json_line_per_record() {
        let dir = std::env::temp_dir().join(format!("incident-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("rate_limit_incidents.jsonl");
        let _ = std::fs::remove_file(&path);

        append_jsonl(&path, &record());
        append_jsonl(&path, &record());

        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2);
        let v: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(v["credential"], 22);
        assert_eq!(v["kind"], "429_transient");
        assert_eq!(v["req_1m"], 12);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn write_failure_is_silent() {
        // 目录不存在且不可创建(以文件占位)→ 写入失败但不得 panic
        let bogus = std::env::temp_dir().join(format!("incident-blocked-{}", std::process::id()));
        std::fs::write(&bogus, b"occupied").unwrap();
        let path = bogus.join("sub").join("x.jsonl");
        append_jsonl(&path, &record()); // 不应 panic
        std::fs::remove_file(&bogus).ok();
    }

    #[test]
    fn baseline_record_omits_incident_only_fields() {
        let r = IncidentRecord {
            kind: "baseline".into(),
            model: None,
            attempt: None,
            cooldown_secs: None,
            ..record()
        };
        let json = serde_json::to_string(&r).unwrap();
        assert!(!json.contains("attempt"));
        assert!(!json.contains("cooldown_secs"));
        assert!(!json.contains("model"));
    }
}
```

- [ ] **Step 3.2: 运行测试确认编译失败**

Run: `cargo test --lib kiro::incident_log 2>&1 | tail -5`
Expected: 编译错误,`append_jsonl` 不存在。

- [ ] **Step 3.3: 实现最小代码**

```rust
/// 追加一行 JSON。任何失败(目录只读 / 磁盘满 / 序列化异常)仅 debug 日志。
pub fn append_jsonl(path: &Path, record: &IncidentRecord) {
    let line = match serde_json::to_string(record) {
        Ok(l) => l,
        Err(e) => {
            tracing::debug!("限速观测记录序列化失败: {}", e);
            return;
        }
    };
    let result = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut f| writeln!(f, "{}", line));
    if let Err(e) = result {
        tracing::debug!("限速观测记录写入失败({}): {}", path.display(), e);
    }
}
```

- [ ] **Step 3.4: 运行测试确认通过**

Run: `cargo test --lib kiro::incident_log 2>&1 | tail -5`
Expected: `3 passed`

- [ ] **Step 3.5: Commit**

```bash
git add src/kiro/mod.rs src/kiro/incident_log.rs
git commit -m "feat(observability): 限速观测记录 JSONL 落盘(best-effort)"
```

---

### Task 4: MultiTokenManager 集成

**Files:**
- Modify: `src/kiro/token_manager.rs`
  - struct 字段(约 :590 `MultiTokenManager`)
  - 构造函数 `MultiTokenManager::new`(约 :640,以实际为准)
  - `add_cost`(约 :1309)
  - `report_rate_limited`(约 :1474)
  - 新增方法 + 测试

- [ ] **Step 4.1: 写失败测试**

在 `token_manager.rs` 测试模块(`mod tests`)末尾加:

```rust
#[test]
fn track_request_start_feeds_in_flight_and_window() {
    let manager =
        MultiTokenManager::new(Config::default(), vec![KiroCredentials::default()], None, None, false)
            .unwrap();

    let g1 = manager.track_request_start(1);
    let g2 = manager.track_request_start(1);
    assert_eq!(manager.in_flight().get(1), (2, 2));

    let stats = manager.window_stats(1);
    assert_eq!(stats.req_1m, 2, "track_request_start 应同时记入活动窗口");

    drop(g1);
    drop(g2);
    assert_eq!(manager.in_flight().get(1), (0, 2));
}

#[test]
fn add_cost_feeds_token_window() {
    let manager =
        MultiTokenManager::new(Config::default(), vec![KiroCredentials::default()], None, None, false)
            .unwrap();
    manager.add_cost(1, "claude-sonnet-4-5", 1000, 200, 300, 50);
    let stats = manager.window_stats(1);
    assert_eq!(stats.tokens_in_1m, 1500, "input+cache_read+cache_creation 都是上游入量");
    assert_eq!(stats.tokens_out_1m, 50);
}

#[test]
fn report_rate_limited_records_429_timestamp() {
    let manager =
        MultiTokenManager::new(Config::default(), vec![KiroCredentials::default()], None, None, false)
            .unwrap();
    assert_eq!(manager.window_stats(1).secs_since_last_429, None);
    manager.report_rate_limited(1, StdDuration::from_secs(60));
    let secs = manager.window_stats(1).secs_since_last_429;
    assert!(matches!(secs, Some(0..=2)));
}

#[test]
fn build_observability_record_includes_all_fields() {
    let manager =
        MultiTokenManager::new(Config::default(), vec![KiroCredentials::default()], None, None, false)
            .unwrap();
    let _g = manager.track_request_start(1);
    let r = manager.build_observability_record(
        1,
        "429_suspicious",
        Some("claude-sonnet-4-5"),
        Some(3),
        Some(600),
    );
    assert_eq!(r.credential, 1);
    assert_eq!(r.kind, "429_suspicious");
    assert_eq!(r.in_flight, 1);
    assert_eq!(r.req_1m, 1);
    assert_eq!(r.model.as_deref(), Some("claude-sonnet-4-5"));
    assert_eq!(r.attempt, Some(3));
    assert_eq!(r.cooldown_secs, Some(600));
    assert!(chrono::DateTime::parse_from_rfc3339(&r.ts).is_ok());
}

#[test]
fn baseline_records_skip_idle_credentials() {
    let manager = MultiTokenManager::new(
        Config::default(),
        vec![KiroCredentials::default(), KiroCredentials::default()],
        None,
        None,
        false,
    )
    .unwrap();
    // 仅凭据 1 有活动
    let _g = manager.track_request_start(1);
    let records = manager.baseline_records();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].credential, 1);
    assert_eq!(records[0].kind, "baseline");
    assert_eq!(records[0].attempt, None);
}
```

- [ ] **Step 4.2: 运行测试确认编译失败**

Run: `cargo test --lib token_manager::tests::track_request_start 2>&1 | tail -5`
Expected: 编译错误,新方法不存在。

- [ ] **Step 4.3: 实现**

a) 顶部 import 区(`use crate::kiro::machine_id;` 附近)加:

```rust
use crate::kiro::activity_window::{ActivityWindow, WindowStats};
use crate::kiro::in_flight::{InFlightGuard, InFlightTracker};
use crate::kiro::incident_log::{self, IncidentRecord};
```

b) `MultiTokenManager` struct 末尾(`convo_sticky` 字段后)加两个字段:

```rust
    /// 在途请求计数(实时并发观测)
    in_flight: InFlightTracker,
    /// 每凭据滚动活动窗口(限速观测);与 entries 锁独立,避免热路径锁竞争。
    /// 锁序约束:绝不在持有 entries 锁时再取本锁(反之亦然),防死锁。
    activity: Mutex<HashMap<u64, ActivityWindow>>,
```

c) `MultiTokenManager::new` 构造的 `Self { ... }` 里加:

```rust
            in_flight: InFlightTracker::default(),
            activity: Mutex::new(HashMap::new()),
```

d) 新增方法(放在 `add_cost` 附近):

```rust
    /// 暴露在途计数器(admin 快照读取)
    pub fn in_flight(&self) -> &InFlightTracker {
        &self.in_flight
    }

    /// 登记一次上游请求尝试:在途 +1(guard 归还)、活动窗口记 start。
    /// 每次"上游 POST"都算一次,故障转移重试各算一次——限速分析关心的
    /// 正是上游视角的请求频率。
    pub fn track_request_start(&self, id: u64) -> InFlightGuard {
        self.activity
            .lock()
            .entry(id)
            .or_default()
            .record_start(Utc::now());
        self.in_flight.track(id)
    }

    /// 该凭据当前窗口统计(测试与观测记录共用)
    pub fn window_stats(&self, id: u64) -> WindowStats {
        self.activity
            .lock()
            .entry(id)
            .or_default()
            .stats(Utc::now())
    }

    /// 组装一条观测记录(事故快照与基线采样共用)
    pub fn build_observability_record(
        &self,
        id: u64,
        kind: &str,
        model: Option<&str>,
        attempt: Option<u32>,
        cooldown_secs: Option<u64>,
    ) -> IncidentRecord {
        let stats = self.window_stats(id);
        let (in_flight, in_flight_peak) = self.in_flight.get(id);
        IncidentRecord {
            ts: Utc::now().to_rfc3339(),
            credential: id,
            kind: kind.to_string(),
            in_flight,
            in_flight_peak,
            req_1m: stats.req_1m,
            req_5m: stats.req_5m,
            tokens_in_1m: stats.tokens_in_1m,
            tokens_out_1m: stats.tokens_out_1m,
            model: model.map(str::to_string),
            attempt,
            secs_since_last_429: stats.secs_since_last_429,
            cooldown_secs,
        }
    }

    /// 事故快照:专用 target 结构化日志 + JSONL 落盘(best-effort)
    pub fn log_rate_limit_incident(
        &self,
        id: u64,
        kind: &str,
        model: Option<&str>,
        attempt: u32,
        cooldown_secs: Option<u64>,
    ) {
        let r = self.build_observability_record(id, kind, model, Some(attempt), cooldown_secs);
        tracing::warn!(
            target: "rate_limit_incident",
            credential = r.credential,
            kind = %r.kind,
            in_flight = r.in_flight,
            in_flight_peak = r.in_flight_peak,
            req_1m = r.req_1m,
            req_5m = r.req_5m,
            tokens_in_1m = r.tokens_in_1m,
            tokens_out_1m = r.tokens_out_1m,
            model = ?r.model,
            attempt = ?r.attempt,
            secs_since_last_429 = ?r.secs_since_last_429,
            cooldown_secs = ?r.cooldown_secs,
            "限速观测事件"
        );
        self.append_observability_record(&r);
    }

    /// 基线采样:返回所有"有活动"凭据的记录(无活动不采,避免无意义数据)
    pub fn baseline_records(&self) -> Vec<IncidentRecord> {
        let ids: Vec<u64> = {
            let entries = self.entries.lock();
            entries.iter().map(|e| e.id).collect()
        };
        ids.into_iter()
            .filter(|id| {
                let stats = self.window_stats(*id);
                let (cur, _) = self.in_flight.get(*id);
                stats.req_1m > 0 || cur > 0
            })
            .map(|id| self.build_observability_record(id, "baseline", None, None, None))
            .collect()
    }

    /// 基线采样落盘(后台任务每 60s 调一次)
    pub fn log_baseline_samples(&self) {
        for r in self.baseline_records() {
            self.append_observability_record(&r);
        }
    }

    fn append_observability_record(&self, r: &IncidentRecord) {
        if let Some(dir) = self.cache_dir() {
            incident_log::append_jsonl(&dir.join("rate_limit_incidents.jsonl"), r);
        }
    }
```

e) `add_cost`(:1309)在 `self.save_stats_debounced();` 之前加(注意:此时 entries 锁已释放,符合锁序约束):

```rust
        self.activity.lock().entry(id).or_default().record_tokens(
            Utc::now(),
            (input.max(0) + cache_read.max(0) + cache_creation.max(0)) as u64,
            output.max(0) as u64,
        );
```

f) `report_rate_limited`(:1474)函数体开头加:

```rust
        self.activity
            .lock()
            .entry(id)
            .or_default()
            .record_429(Utc::now());
```

- [ ] **Step 4.4: 运行测试确认通过**

Run: `cargo test --lib token_manager 2>&1 | tail -5`
Expected: 全部通过(含既有测试)。

- [ ] **Step 4.5: Commit**

```bash
git add src/kiro/token_manager.rs
git commit -m "feat(observability): MultiTokenManager 集成在途计数/活动窗口/观测记录"
```

---

### Task 5: provider 接线(guard 贯穿 + 事故快照调用点)

**Files:**
- Modify: `src/kiro/provider.rs`
  - `call_api`(:228)/ `call_api_stream`(:344)/ `call_api_with_retry`(:544)签名
  - 重试循环内 acquire 之后、各错误分支
  - `call_mcp_with_retry`(:360 附近)、`send_once_with_credential`(:245)
- Modify: `src/anthropic/handlers.rs` 三个调用点(:439、:583、:1012)与 `create_sse_stream`(:476)
- Modify: `src/anthropic/websearch.rs`(若 `call_mcp` 调用处解构变化;本计划保持 `call_mcp` 签名不变,无需改)

- [ ] **Step 5.1: 修改 provider 签名与重试循环**

a) `call_api_with_retry` 返回类型 `anyhow::Result<(reqwest::Response, u64)>` 改为:

```rust
    ) -> anyhow::Result<(reqwest::Response, u64, crate::kiro::in_flight::InFlightGuard)> {
```

`call_api`(:228)与 `call_api_stream`(:344)的返回类型同步改为相同三元组(它们只是透传)。

b) 重试循环内,`let config = self.token_manager.config();` 之前加:

```rust
            // 每次上游尝试都登记:在途 +1(guard 随本轮作用域 drop 自动归还)、窗口记 start
            let in_flight_guard = self.token_manager.track_request_start(ctx.id);
```

c) 成功分支 `return Ok((response, ctx.id));` 改为:

```rust
                return Ok((response, ctx.id, in_flight_guard));
```

(失败分支不需要显式 drop——`in_flight_guard` 随每轮循环作用域结束自动归还。)

- [ ] **Step 5.2: 在 429/402/40x 分支插入事故快照**

事故快照必须在 `report_*` **之前**调用:`report_rate_limited` 会刷新 last_429 时间戳,先 report 会把 `secs_since_last_429` 变成 0,丢失"距上一次 429 间隔"信息。

a) 429 分支(`if status.as_u16() == 429 {`,约 :695),在 `let has_available = self.token_manager.report_rate_limited(...)` 之前加:

```rust
                self.token_manager.log_rate_limit_incident(
                    ctx.id,
                    if is_suspicious { "429_suspicious" } else { "429_transient" },
                    model.as_deref(),
                    attempt as u32 + 1,
                    Some(cooldown.as_secs()),
                );
```

b) 402 额度分支(`if status.as_u16() == 402 && endpoint.is_monthly_request_limit(&body) {`,约 :633),在 `report_quota_exhausted` 之前加:

```rust
                self.token_manager.log_rate_limit_incident(
                    ctx.id,
                    "402_quota",
                    model.as_deref(),
                    attempt as u32 + 1,
                    None,
                );
```

c) 401/403 分支(`if matches!(status.as_u16(), 401 | 403) {`,约 :660),在 force-refresh 逻辑之后、`report_failure` 之前加:

```rust
                self.token_manager.log_rate_limit_incident(
                    ctx.id,
                    "40x_auth",
                    model.as_deref(),
                    attempt as u32 + 1,
                    None,
                );
```

d) `call_mcp_with_retry` 循环内 acquire ctx 成功后同样加 `let _in_flight_guard = self.token_manager.track_request_start(ctx.id);`(guard 留在循环作用域内即可——MCP 响应体小,覆盖到响应头返回已足够)。

e) `send_once_with_credential`(:245)在 `acquire_context_for_id` 成功后加同样一行(验证请求也是真实上游调用,应计入)。

- [ ] **Step 5.3: 修改 handlers 三个调用点与 create_sse_stream**

a) 非流式(:583 附近):

```rust
    let (response, credential_id, _in_flight_guard) = match provider
        .call_api(request_body, Some(&conversation_id))
        .await
    {
        Ok(resp) => resp,
        Err(e) => return map_provider_error(e),
    };
```

(`_in_flight_guard` 绑定存活到函数结束,自然覆盖 body 读取。注意必须命名为 `_in_flight_guard` 而非 `_`——`_` 会立即 drop。)

b) 流式 `handle_stream_request`(:438 附近)与 `/cc` 流式(:1012 附近),两处同样解构出 `in_flight_guard`,并传给 `create_sse_stream`:

```rust
    let (response, credential_id, in_flight_guard) = match provider
        .call_api_stream(request_body, Some(&conversation_id))
        .await
    {
        Ok(resp) => resp,
        Err(e) => return map_provider_error(e),
    };
    // ...
    let stream = create_sse_stream(response, ctx, initial_events, in_flight_guard);
```

c) `create_sse_stream`(:476)签名加参数,并在最终返回的组合流上把 guard move 进 `map` 闭包(闭包随流一起 drop,覆盖播完/客户端断开两种结束方式):

```rust
fn create_sse_stream(
    response: reqwest::Response,
    ctx: StreamContext,
    initial_events: Vec<SseEvent>,
    in_flight_guard: crate::kiro::in_flight::InFlightGuard,
) -> impl Stream<Item = Result<Bytes, Infallible>> {
```

函数最后(现有最终组合流表达式,记为 `combined`)改为:

```rust
    combined.map(move |item| {
        // guard 被 move 进闭包:流 drop(播完或客户端断开)时计数自动归还
        let _keep = &in_flight_guard;
        item
    })
```

(`futures::StreamExt` 已在该文件使用 `stream::iter`/`unfold`,若 `map` 未引入则补 `use futures::StreamExt;`。)

- [ ] **Step 5.4: 运行全量测试**

Run: `cargo test --quiet 2>&1 | tail -3`
Expected: 全部通过。若 handlers/websearch 还有未改的解构点,编译器会逐个指出——按 a/b 模式修复。

- [ ] **Step 5.5: Commit**

```bash
git add src/kiro/provider.rs src/anthropic/handlers.rs
git commit -m "feat(observability): guard 贯穿请求生命周期,429/402/40x 分支打事故快照"
```

---

### Task 6: 基线采样后台任务

**Files:**
- Modify: `src/main.rs`(`kiro_provider` 创建之后,约 :150)

- [ ] **Step 6.1: 添加后台任务**

`let kiro_provider = Arc::new(...)` 语句块之后加:

```rust
    // 限速观测基线采样:每 60s 为有活动的凭据落一行 baseline 记录,
    // 作为 429 事故数据的对照组(详见 docs/superpowers/specs/2026-06-12-*.md)
    {
        let tm = token_manager.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
            interval.tick().await; // 第一次 tick 立即返回,跳过
            loop {
                interval.tick().await;
                tm.log_baseline_samples();
            }
        });
    }
```

- [ ] **Step 6.2: 编译验证**

Run: `cargo build 2>&1 | tail -3`
Expected: 编译通过。(后台循环本身不可单测,正确性由 Task 4 的 `baseline_records_skip_idle_credentials` 保证。)

- [ ] **Step 6.3: Commit**

```bash
git add src/main.rs
git commit -m "feat(observability): 基线采样后台任务(60s tick)"
```

---

### Task 7: admin API 透出并发字段

**Files:**
- Modify: `src/kiro/token_manager.rs`:`snapshot()`(:1600)与 `CredentialEntrySnapshot` 定义(同文件,搜 `struct CredentialEntrySnapshot`)
- Modify: `src/admin/types.rs`:`CredentialStatusItem`(:24)、`CredentialsStatusResponse`(:10)
- Modify: `src/admin/service.rs`:`get_all_credentials`(:67)

- [ ] **Step 7.1: 写失败测试**

`token_manager.rs` 测试模块加:

```rust
#[test]
fn snapshot_includes_in_flight_fields() {
    let manager =
        MultiTokenManager::new(Config::default(), vec![KiroCredentials::default()], None, None, false)
            .unwrap();
    let _g = manager.track_request_start(1);
    let snap = manager.snapshot();
    let e = snap.entries.iter().find(|e| e.id == 1).unwrap();
    assert_eq!(e.in_flight, 1);
    assert_eq!(e.in_flight_peak, 1);
}
```

Run: `cargo test --lib snapshot_includes_in_flight 2>&1 | tail -3`
Expected: 编译错误,字段不存在。

- [ ] **Step 7.2: 实现**

a) `CredentialEntrySnapshot` struct 加字段:

```rust
    pub in_flight: u32,
    pub in_flight_peak: u32,
```

b) `snapshot()`(:1600)在 `let now = Utc::now();` 附近加 `let in_flight = self.in_flight.snapshot();`,map 闭包里加:

```rust
                    in_flight: in_flight.get(&e.id).map(|v| v.0).unwrap_or(0),
                    in_flight_peak: in_flight.get(&e.id).map(|v| v.1).unwrap_or(0),
```

c) `src/admin/types.rs` `CredentialStatusItem`(`success_count` 字段后)加:

```rust
    /// 当前在途请求数(实时并发)
    pub in_flight: u32,
    /// 进程启动以来最高瞬时并发
    pub in_flight_peak: u32,
```

`CredentialsStatusResponse`(`available` 字段后)加:

```rust
    /// 全局在途请求总数
    pub total_in_flight: u32,
```

d) `src/admin/service.rs` `get_all_credentials` map 里加两个字段透传;`CredentialsStatusResponse` 构造前加:

```rust
        let total_in_flight: u32 = credentials.iter().map(|c| c.in_flight).sum();
```

并在构造里加 `total_in_flight,`。

- [ ] **Step 7.3: 运行测试**

Run: `cargo test --quiet 2>&1 | tail -3`
Expected: 全部通过。

- [ ] **Step 7.4: Commit**

```bash
git add src/kiro/token_manager.rs src/admin/types.rs src/admin/service.rs
git commit -m "feat(observability): admin API 透出每凭据在途并发/峰值"
```

---

### Task 8: admin UI(并发列 + 秒级轮询)

**Files:**
- Modify: `admin-ui/src/types/api.ts`(:23 附近 `CredentialStatusItem`)
- Modify: `admin-ui/src/hooks/use-credentials.ts`(:20)
- Modify: `admin-ui/src/components/dashboard.tsx`(:736 表头)
- Modify: `admin-ui/src/components/credential-row.tsx`(:377 前插入单元格)

- [ ] **Step 8.1: TS 类型**

`types/api.ts` `CredentialStatusItem` 的 `successCount` 后加:

```typescript
  inFlight: number
  inFlightPeak: number
```

`CredentialsStatusResponse`(同文件,搜 `total:`)加:

```typescript
  totalInFlight: number
```

- [ ] **Step 8.2: 轮询间隔**

`use-credentials.ts:20`:

```typescript
    refetchInterval: 2000, // 秒级并发观测;React Query 默认窗口失焦即停轮
```

- [ ] **Step 8.3: 表头与单元格**

`dashboard.tsx` 表头"状态"列 `<th>` 后插入:

```tsx
                      <th className="px-2 py-2 text-left font-medium">并发</th>
```

`credential-row.tsx`:在"计数 / 用量"单元格(`<td className="px-2 py-2 text-sm whitespace-nowrap">`,约 :377,对照表头顺序确认)**之前**插入:

```tsx
        <td className="px-2 py-2 text-sm whitespace-nowrap">
          <span className={credential.inFlight > 0 ? 'font-medium text-green-600' : 'text-muted-foreground'}>
            {credential.inFlight}
          </span>
          <span className="text-xs text-muted-foreground"> / {credential.inFlightPeak}</span>
        </td>
```

- [ ] **Step 8.4: 构建前端并验证**

Run: `cd admin-ui && npm run build 2>&1 | tail -3 && cd ..`
Expected: 构建成功(产物进 `admin-ui/dist`,由 rust-embed 在 cargo build 时嵌入)。
Run: `cargo build 2>&1 | tail -3`
Expected: 编译通过。

- [ ] **Step 8.5: Commit**

```bash
git add admin-ui/src admin-ui/dist
git commit -m "feat(observability): admin UI 并发列(当前/峰值),轮询 2s"
```

(若 `admin-ui/dist` 在 .gitignore 中则只提交 `admin-ui/src`,以仓库现状为准。)

---

### Task 9: 全量验证

- [ ] **Step 9.1: 全量测试 + 构建**

Run: `cargo test --quiet 2>&1 | tail -3 && cargo build --release 2>&1 | tail -3`
Expected: 测试全过、构建成功。

- [ ] **Step 9.2: 本地冒烟**

```bash
# 起服务(用本地 config),发一条消息,然后:
curl -s http://127.0.0.1:8990/api/admin/credentials -H "x-api-key: <admin key>" | jq '.totalInFlight, .credentials[0].inFlight, .credentials[0].inFlightPeak'
# 等 60s+,确认基线行出现:
tail -2 config/rate_limit_incidents.jsonl
```

Expected: 字段存在;请求过后 `inFlightPeak >= 1`;JSONL 出现 `"kind":"baseline"` 行。

- [ ] **Step 9.3: 最终提交(如有冒烟期间的修补)**

```bash
git status --short  # 确认无遗漏
```

---

## 验收对照(spec → task)

| Spec 要求 | Task |
|---|---|
| 组件 1 InFlightTracker + RAII | 1, 5 |
| 组件 2 滚动活动窗口 | 2, 4 |
| 组件 3 事故快照(日志 + JSONL) | 3, 4, 5 |
| 组件 4 基线采样 | 4, 6 |
| 组件 5 admin API/UI | 7, 8 |
| 流式 guard 存活范围(风险点 1) | 1(Step 1.1 idiom 测试), 5(Step 5.3c) |
| JSONL 静默降级(风险点 3) | 3(Step 3.1 write_failure_is_silent) |
