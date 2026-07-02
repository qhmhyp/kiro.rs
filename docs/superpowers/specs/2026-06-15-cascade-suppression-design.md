# 级联抑制(Cascade Suppression)设计

日期:2026-06-15
状态:已与用户确认
分支:feat/opus-4-7-support

## 背景

120 起生产 429 的观测数据(3 天,4736 条记录)表明:
- 38% 的事故是**跨凭据级联**:凭据 A 被冷却 60 秒 → 全部流量倒给凭据 B →
  B 在途并发飙升 → 28 秒(中位数)后 B 也 429 → 两凭据同时冷却 → 客户端
  收到"所有凭据已用尽"的 429。
- 级联方向完全对称(22→23 计 20 次,23→22 计 21 次),同凭据连续 429 仅 3 次。
- 首发 429 不可控(上游账号级 token 令牌桶),但级联第二发是**自家故障转移制造的**。

本设计消灭级联,不治首发;首发靠扩池(长期)。

## 数据依据(设计参数来源)

| 指标 | 数值 | 设计参数 |
|---|---|---|
| 首发 in_flight p90 | 3 | 并发软上限 = 3 |
| 跟随 in_flight p50 / max | 3 / 8 | 上限有效削峰空间 |
| 跟随距首发 p50 / p75 | 28s / 41s | 冷却后回流需柔性 ramp |
| 连续安全运行 in_flight ≤ 3 | 40 分钟 | 上限 3 不影响正常流量 |
| opus 4.6 级联率 49% | 最高 | 大模型流式请求最受益 |

## 三个机制

### 机制 1:单凭据并发软上限

配置项 `maxInFlightPerCredential`(默认 3,`0` = 关闭)。

**选择凭据时**(`acquire_credential` / `acquire_credential_sticky`):
- 可用性判定追加:`in_flight(id) < cap`;
- 粘性命中路径同样检查——满载即视为"本次不可用",走现有 fallback
  (切空闲凭据并重新绑定粘性映射,符合用户选择的"立刻切"语义);
- 非粘性 LRU 路径:在同优先级组内优先选 in_flight 最小者
  (若 in_flight 相同则退回 LRU);

**全池满载**(所有凭据 in_flight ≥ cap 且存在"仅因满载而不可选"的凭据):
- `acquire_context` 进入等待循环:
  `tokio::time::timeout(剩余超时, notify.notified())`→ 被唤醒后重试选择;
- 总超时 10 秒(常量,先不进配置);
- 超时后**降级直选**:忽略 cap,在可用凭据中选 in_flight 最小者(LRU 决胜);
- 降级时 `tracing::info!(target: "rate_limit_incident", kind="overcap", ...)`;
- 语义:永远不比现状更差——最坏退化为今天的行为。

**唤醒机制**:`InFlightGuard` drop 时调 `Notify::notify_waiters()`
(一个共享的 `Arc<tokio::sync::Notify>`)。已有 `InFlightTracker` 的 `Drop`
实现只做原子 `fetch_sub`,追加一行 notify 调用。

### 机制 2:冷却柔性回流

凭据走出 `cooldown_until` 后的前 30 秒,`effective_cap = 1`(探路模式);
30 秒后恢复满 cap。

实现:选择时从 `cooldown_until` 与当前时间差计算,无新状态字段。

### 机制 3:冷却抖动

`report_rate_limited` 设置 `cooldown_until` 时,在基础冷却(60s)上
加 0-15 秒伪随机抖动(用当前毫秒时间戳对 16 取模,不引入 rand 依赖):

```
cooldown = base + (now_millis % 16) as seconds
```

作用:打散两凭据同步冷却 → 同步解冻 → 同步被打满的乒乓节奏。

## 影响范围

| 路径 | 影响 |
|---|---|
| `call_api_with_retry` | 自动生效(走 `acquire_context`) |
| `call_mcp_with_retry` | 自动生效 |
| `send_once_with_credential` | **不受限**——指名道姓直选,管理端验证用 |
| admin UI | 无需改动(并发列已能看到削峰效果) |
| 现有重试逻辑 | 不动——重试循环的每轮 acquire 自然受 cap 约束 |

## 配置

`config.json` 新增可选字段:

```json
{
  "maxInFlightPerCredential": 3
}
```

不配 = 默认 3;配 0 = 关闭(等价于 cap=∞,回退现状)。

## 不做(YAGNI)

- 按模型区分 cap(opus 比 haiku 更"重"——但上限 3 已经够,opus 级联率高
  是因为它请求慢占槽久,cap 本身已自然限制)
- 排队公平性/优先级(10 秒内的短暂排队不需要复杂调度)
- 客户端节流(按 token 预算主动限速——后续独立功能,级联抑制不依赖)

## 测试

1. cap 过滤:满载凭据不被选中
2. 粘性满载即切:粘住的凭据满载时选空闲凭据并重新绑定
3. 全池满载等待 → 空位 Notify 唤醒(tokio 时间暂停测试)
4. 超时降级直选:10 秒超时后选 in_flight 最小者
5. 冷却后 ramp:刚解冻 effective_cap=1 → 30 秒后恢复满 cap
6. 冷却抖动:基础 60s + 0-15s 抖动,范围断言
7. cap=0 时全部机制关闭,行为与现状一致
8. 回归:现有的 LRU 选择/粘性/故障转移/自愈测试全部保持通过

## 预期效果

- 级联第二发(当前 46/120 起)应降至接近零(cap=3 压住倒流量峰值)
- 客户端可见的"全池耗尽"窗口(两凭据同时冷却)大幅收窄
- 正常流量零影响(连续 40 分钟安全运行时 in_flight 从未超过 3)
- opus 长流式请求自然受益最大(级联率从 49% 预期降至个位)
