#!/usr/bin/env python3
"""分析 rate_limit_incidents.jsonl:验证"token 令牌桶"假说。

用法:
  python3 tools/analyze_rate_limits.py /path/to/rate_limit_incidents.jsonl
  # 或在 VPS 上:
  ssh <vps> python3 - /app/config/rate_limit_incidents.jsonl < tools/analyze_rate_limits.py

核心问题:429 发生前 N 分钟该凭据的 token 累计量,是否显著高于
"安全分钟"(其后 5 分钟内没有 429)的同指标?是 → 配额/令牌桶坐实。
"""
import json
import sys
from datetime import datetime, timedelta


def parse(path):
    rows = []
    with open(path) as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            r = json.loads(line)
            r["t"] = datetime.fromisoformat(r["ts"].replace("Z", "+00:00"))
            rows.append(r)
    return rows


def pct(sorted_vals, p):
    if not sorted_vals:
        return 0
    i = min(len(sorted_vals) - 1, int(len(sorted_vals) * p))
    return sorted_vals[i]


def fmt_dist(vals):
    s = sorted(vals)
    return (
        f"n={len(s)} p25={pct(s, 0.25):,} p50={pct(s, 0.5):,} "
        f"p75={pct(s, 0.75):,} p90={pct(s, 0.9):,} max={s[-1]:,}" if s else "n=0"
    )


def main(path):
    rows = parse(path)
    incidents = [r for r in rows if r["kind"] != "baseline"]
    baselines = [r for r in rows if r["kind"] == "baseline"]

    print(f"== 总览 ==")
    print(f"记录 {len(rows)} 条,事故 {len(incidents)} 起,基线 {len(baselines)} 条")
    print(f"时间范围 {rows[0]['ts'][:19]} ~ {rows[-1]['ts'][:19]}")

    by_kind, by_cred, by_model, by_hour = {}, {}, {}, {}
    for i in incidents:
        by_kind[i["kind"]] = by_kind.get(i["kind"], 0) + 1
        by_cred[i["credential"]] = by_cred.get(i["credential"], 0) + 1
        m = i.get("model") or "?"
        by_model[m] = by_model.get(m, 0) + 1
        h = i["ts"][11:13]
        by_hour[h] = by_hour.get(h, 0) + 1
    print(f"按类型: {by_kind}")
    print(f"按凭据: {by_cred}")
    print(f"按模型: {by_model}")
    print(f"按小时(UTC): {dict(sorted(by_hour.items()))}")

    inflight = sorted(i["in_flight"] for i in incidents)
    print(f"事故时刻 in_flight 分布: {fmt_dist(inflight)}")

    # 首发 vs 跟随(同一 60s 窗口内的后续事故视为级联跟随)
    firsts, followers = [], []
    last_t = None
    for i in sorted(incidents, key=lambda x: x["t"]):
        if last_t is not None and (i["t"] - last_t).total_seconds() < 60:
            followers.append(i)
        else:
            firsts.append(i)
        last_t = i["t"]
    print(f"首发 {len(firsts)} 起 / 级联跟随 {len(followers)} 起")
    print(f"首发 in_flight: {fmt_dist(sorted(f['in_flight'] for f in firsts))}")
    print(f"跟随 in_flight: {fmt_dist(sorted(f['in_flight'] for f in followers))}")

    # 核心检验:事发前 N 分钟该凭据 token 累计(用基线逐分钟采样求和近似)
    for window_min in (5, 10):
        win = timedelta(minutes=window_min + 0.6)

        def tokens_before(cred, t):
            return sum(
                b["tokens_in_1m"]
                for b in baselines
                if b["credential"] == cred and timedelta(0) < t - b["t"] <= win
            )

        incident_sums = sorted(
            tokens_before(i["credential"], i["t"]) for i in firsts
        )
        # 安全分钟:该基线之后 5 分钟内该凭据没有任何事故
        def is_safe(b):
            return not any(
                i["credential"] == b["credential"]
                and timedelta(0) <= i["t"] - b["t"] <= timedelta(minutes=5)
                for i in incidents
            )

        safe_sums = sorted(
            tokens_before(b["credential"], b["t"])
            for b in baselines
            if is_safe(b)
        )
        print(f"\n== 事发前 {window_min} 分钟 tokens_in 累计(仅首发)==")
        print(f"事故前: {fmt_dist(incident_sums)}")
        print(f"安全分钟: {fmt_dist(safe_sums)}")

    # 风暴/静默分段(相邻事故间隔 > 10 分钟视为静默边界)
    print(f"\n== 风暴/静默分段 ==")
    seq = sorted(incidents, key=lambda x: x["t"])
    seg_start, prev = seq[0], seq[0]
    segs = []
    for i in seq[1:]:
        gap = (i["t"] - prev["t"]).total_seconds()
        if gap > 600:
            segs.append((seg_start["ts"][11:19], prev["ts"][11:19], gap))
            seg_start = i
        prev = i
    segs.append((seg_start["ts"][11:19], prev["ts"][11:19], None))
    for s, e, gap_after in segs:
        tail = f" → 静默 {gap_after/60:.0f} 分钟" if gap_after else ""
        print(f"风暴 {s} ~ {e}{tail}")


if __name__ == "__main__":
    main(sys.argv[1])
