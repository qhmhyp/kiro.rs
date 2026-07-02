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

/// 追加一行 JSON。任何失败(目录只读 / 磁盘满 / 序列化异常)仅 debug 日志。
///
/// 同步写入 < 1ms(本地盘、无 fsync),在 async 上下文直接调用无需 spawn_blocking;
/// 若未来写入目标为网络挂载卷需重评。
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
        // writeln! 会产生两次 write syscall(内容+换行),O_APPEND 只保证单次 write 原子;
        // 并发写会撕行。拼好再 write_all 只有一次 syscall,保证原子性。
        .and_then(|mut f| f.write_all(format!("{line}\n").as_bytes()));
    if let Err(e) = result {
        tracing::debug!("限速观测记录写入失败({}): {}", path.display(), e);
    }
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
        // secs_since_last_429 在 baseline 中有意保留(活动窗口的正常输出),不属于"仅事故"字段
        assert!(!json.contains("attempt"));
        assert!(!json.contains("cooldown_secs"));
        assert!(!json.contains("model"));
    }
}
