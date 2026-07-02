# 凭据限速观测(Rate Limit Observability)设计

日期:2026-06-12
状态:已与用户确认
分支:feat/opus-4-7-support

## 背景与目标

生产环境频繁出现上游 429(含风控类"suspicious activity")乃至凭据被封禁(#15),
但目前没有任何数据能回答:**单个凭据在什么负载条件下会触发 Kiro 限速?**

本设计为被动观察方案:在生产流量中记录限速事件的"案发现场"与日常负载基线,
攒数据后离线对照分析,找出触发维度(并发数 / 请求频率 / token 吞吐 / 模型 / 重试模式)。

明确排除主动压测:高频探测模式正是 Kiro 风控的目标特征,有凭据被封的真实风险。

## 方法论

只记录事故数据无法得出阈值(可能平时负载也一样高)。因此三件套:

1. **事故快照**:429/402/403 发生瞬间,记录该凭据的负载现场;
2. **基线采样**:每 60 秒记录活跃凭据的负载,作为"没出事"的对照组;
3. **离线分析**:jq 对照两组数据的分布差异。

## 组件设计

### 组件 1:InFlightTracker(实时并发计数)

位置:`src/kiro/token_manager.rs`(或同目录独立小模块)。

```rust
pub struct InFlightTracker {
    counters: Mutex<HashMap<u64, Arc<InFlightCounter>>>, // 仅插入时短锁
}
pub struct InFlightCounter {
    current: AtomicU32,
    peak: AtomicU32, // 进程启动以来最高瞬时并发,不持久化
}
pub struct InFlightGuard(Arc<InFlightCounter>); // Drop 时 current -= 1
```

- 计数语义:一次在途请求 = 凭据被选中 → 上游响应**完全消费完毕**
  (非流式:body 读完;流式:SSE 流播完或客户端断开)。
- `MultiTokenManager::acquire_context`(`token_manager.rs:940`)及
  `acquire_context_for_id`(`token_manager.rs:1007`)成功时生成 guard,
  `CallContext`(`token_manager.rs:621`)新增 guard 字段。
- 故障转移:`call_api_with_retry`(`provider.rs:544`)重试循环中,每轮失败
  `CallContext` 随作用域 drop,计数自动归还,同一请求不会双计。
- 成功路径:`call_api_with_retry` 返回值从 `(reqwest::Response, u64)` 改为携带
  guard;handler 非流式路径 guard 活到函数结束,流式路径 guard **move 进最终
  交给 axum 的 SSE 流对象**,流 drop 时归还。
- websearch 与凭据 verify 走同一 acquire 路径,自动覆盖。

选择 RAII 而非 acquire/report 配对计数的原因:客户端断流、early return、
panic、重试切换凭据等路径都会让配对计数漏减漂移;guard 的 Drop 是语言级保证。

### 组件 2:每凭据滚动活动窗口

位置:`token_manager.rs`,与现有按凭据统计字段同锁粒度。

- 每凭据一个环形队列(VecDeque,插入时修剪),保留最近 15 分钟事件:
  - 请求开始时间戳(guard 创建处记录);
  - 请求完成时间戳 + 该请求 token 用量(挂在现有按凭据 token 统计入口,
    即更新 `input_tokens_total` 等字段的同一调用点);
  - 最近一次 429 时间戳。
- 可随时导出:`req_1m`、`req_5m`、`tokens_in_1m`、`tokens_out_1m`、
  `secs_since_last_429`。
- 内存:每凭据上限几 KB(15 分钟窗口按条数上限兜底,如 2048 条)。

### 组件 3:事故快照(核心)

触发点:`provider.rs` 的 429 分支(约 `provider.rs:700`)、402 配额分支、
401/403 分支,在冷却/禁用凭据**之前**取快照。

输出两路:

1. 专用结构化日志(独立 target,便于过滤):
   `tracing::warn!(target: "rate_limit_incident", ...)`;
2. JSONL 追加到 `{config_dir}/rate_limit_incidents.jsonl`,
   **best-effort**:写失败仅 debug 日志,绝不影响请求处理;同步小写入,
   单行 append,无需额外依赖。

记录字段:

```json
{
  "ts": "2026-06-12T03:00:00Z",
  "credential": 22,
  "kind": "429_transient | 429_suspicious | 402_quota | 40x_auth",
  "in_flight": 3,
  "in_flight_peak": 7,
  "req_1m": 12,
  "req_5m": 41,
  "tokens_in_1m": 85000,
  "tokens_out_1m": 12000,
  "model": "claude-sonnet-4-5",
  "attempt": 2,
  "secs_since_last_429": 183,
  "cooldown_secs": 60
}
```

### 组件 4:基线采样

- 后台 tokio 任务,每 60 秒 tick;
- 仅对窗口内有活动(`req_1m > 0` 或 `in_flight > 0`)的凭据,
  追加同结构 JSONL 行,`kind: "baseline"`,`cooldown_secs`/`attempt` 置空;
- 与组件 3 写同一个文件,靠 `kind` 区分。

### 组件 5:admin API / UI(沿用 v1 已确认内容)

- `CredentialStatusItem`(`admin/types.rs:24`)新增 `in_flight`、`in_flight_peak`;
- `CredentialsStatusResponse`(`admin/types.rs:10`)新增 `total_in_flight`;
- admin-ui 凭据表新增"并发"列,显示 `当前 / 峰值`,当前 >0 时高亮;
- `use-credentials.ts:16` 轮询间隔 30000 → 2000(React Query 默认失焦停轮)。

## 分析方法(文档即交付,不写代码)

攒 2-3 天数据后:

```bash
# 429 事件 vs 基线的 req_1m 分布对照(按凭据)
jq -s 'group_by(.kind | startswith("429")) | map({kind: .[0].kind, n: length,
  req_1m_p50: (map(.req_1m) | sort | .[length/2|floor]),
  in_flight_max: (map(.in_flight) | max)})' config/rate_limit_incidents.jsonl
```

风控 429(`429_suspicious`)与瞬态 429 分开统计;如需更复杂分析,届时再写
一次性脚本。

## 测试

- guard:增减正确、峰值跟踪、Err 提前返回路径归还、并发 N 任务时 current==N;
- 流式归还:构造含 guard 的流,drop 流(模拟客户端断开)→ 计数归零;
- 滚动窗口:修剪正确性、各统计量计算、条数上限兜底;
- 事故快照:mock 429 路径产出全部字段;JSONL 写失败(只读目录)不影响请求;
- 基线采样:无活动凭据不产生行;
- admin 快照含新字段。

## 明确不做(YAGNI)

前端图表、时间序列数据库、SSE 推送、主动压测、JSONL 轮转(数据量极小,
手动清理即可)。

## 风险点

1. **流式 guard 存活范围**:必须 move 进最终交给 axum 的流对象,不能停在中间
   临时变量;实现时写专门测试钉住。
2. **窗口锁竞争**:活动窗口与现有统计共用锁,插入是 O(1)+修剪,热路径影响
   可忽略;若实测有竞争,降级为每凭据独立小锁。
3. **JSONL 落盘**:挂载目录只读/磁盘满时必须静默降级(仅 tracing 日志),
   不能影响代理主流程。
