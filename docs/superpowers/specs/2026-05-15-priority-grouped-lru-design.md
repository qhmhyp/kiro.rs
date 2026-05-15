# 优先级分组 + 组内 LRU 凭据调度

**Status:** Approved · **Date:** 2026-05-15

## 背景与动机

当前 `MultiTokenManager` 提供两种调度模式：

- `priority`：粘住 `current_id`，仅在当前凭据失败/禁用时切换。同 priority 凭据中只有"第一条"会被使用，其余完全闲置。
- `balanced`（Least-Used）：按累计 `success_count` 选最少的。`success_count` 跨凭据**累计**且**不会归零**——后加入的凭据 `success_count = 0`，会独占流量直到追平历史最高值；老凭据则被冷落。

这两种模式都没有实现"同优先级凭据之间的真实均衡"。本设计用一个统一算法替换它们。

## 设计原则

1. 单一调度算法，无模式开关。
2. `priority` 数值同时表达"分组"和"优先顺序"：同 priority 一组，组内均衡；组全挂时下沉到下一组。
3. 组内均衡使用 LRU（按 `last_used_at` 最旧优先），不依赖累计计数，**新加入凭据自然立刻参与**。
4. 顺手修复"自动禁用状态不立即持久化"的窗口期问题。

## 算法

新增函数（替换 `select_next_credential`）：

```text
acquire_credential(model) -> Option<(id, credentials)>
  1. 在写锁内过滤可用集合：!disabled 且（非 opus 或 supports_opus）
  2. target_priority = 可用集合中最小的 priority
  3. 在 target_priority 组内挑 last_used_at 最旧者
     - last_used_at = None 视为最旧（新凭据天然优先）
     - Some(s1) 与 Some(s2) 按字典序比较（RFC3339 可字典序排序）
  4. 立刻把 chosen.last_used_at = now（同一写锁内完成，避免并发双选）
  5. 解锁后 save_stats_debounced()
```

并发安全性来自"写锁内完成过滤 + 打点"。两个并发请求一定看到不同的 `last_used_at` 时间戳。

## 语义变化：`last_used_at`

由"上次完成请求时间"改为"上次被选中时间"：

- 选中即写入，不论后续请求成功或失败。
- 与运维直觉一致："最近被尝试过的凭据"，便于在 Admin UI 排查。
- 失败回调（`report_failure` 等）不再次写入 `last_used_at`——选中那一刻已经写过。

## 字段与类型变更

### `MultiTokenManager`

删除：
- `current_id: Mutex<u64>`
- `load_balancing_mode: Mutex<String>`

删除的方法：
- `select_next_credential`
- `select_highest_priority`
- `switch_to_next`
- `get_load_balancing_mode`
- `set_load_balancing_mode`
- `persist_load_balancing_mode`

新增方法：
- `acquire_credential(model) -> Option<(u64, KiroCredentials)>`：上述算法
- `self_heal_too_many_failures()`：从 `acquire_context` 中抽出原有自愈逻辑，避免内联

### `CredentialEntry`

保留：所有现有字段。`success_count` 不再参与调度，仅作为统计信息持久化和展示。

### `Config`

`load_balancing_mode: String` 字段保留为：

```rust
#[serde(default, skip_serializing_if = "Option::is_none")]
pub load_balancing_mode_deprecated: Option<String>,
```

旧配置仍可解析，但启动时若该字段非空打一次 warn 日志：`loadBalancingMode 已弃用，将被忽略`。下次 `save()` 不写回。

### `ManagerSnapshot`

`current_id` 字段保留，运行时按下式计算：

```rust
current_id = entries.iter()
    .filter(|e| e.last_used_at.is_some())
    .max_by_key(|e| e.last_used_at.clone())
    .map(|e| e.id)
    .unwrap_or(0)
```

含义变成"最近一次被选中的凭据 ID"，对 Admin UI 透明。

## Admin API 变更

删除：
- `GET /api/admin/load-balancing-mode`
- `POST /api/admin/load-balancing-mode`

相关类型 `SetLoadBalancingModeRequest`、`LoadBalancingModeResponse` 一并删除。其他 Admin API 不变。

`set_priority` 简化：只改 priority + 持久化，不再调用 `select_highest_priority`（新模型下一次 acquire 自动按新 priority 选择）。

## 失败处理路径

`report_failure` / `report_quota_exhausted` / `report_refresh_failure` / `report_refresh_token_invalid`：

1. 删除「设置 `current_id` 切换到下一优先级凭据」的代码块（死代码）。
2. **新增（附加 1）**：当且仅当本次调用导致 `disabled` 从 false 翻成 true 时，调用 `persist_credentials()`。失败记 warn，不影响返回值与函数语义。

```rust
// 末尾，save_stats_debounced 之前
if just_disabled {
    if let Err(e) = self.persist_credentials() {
        tracing::warn!("自动禁用后持久化失败（不影响本次请求）: {}", e);
    }
}
```

## `acquire_context` 主循环

简化后：

```rust
loop {
    if attempts >= max_attempts { bail!(...); }

    let (id, creds) = match self.acquire_credential(model) {
        Some(x) => x,
        None => {
            self.self_heal_too_many_failures();
            match self.acquire_credential(model) {
                Some(x) => x,
                None => bail!("所有凭据均已禁用 ({}/{})", 0, total),
            }
        }
    };

    match self.try_ensure_token(id, &creds).await {
        Ok(ctx) => return Ok(ctx),
        Err(e) => { /* 同现有逻辑：分类 + report_* + attempts++ */ }
    }
}
```

净删除约 30 行（current_id 短路 + 自愈分支）。

## 测试覆盖

### 新增

1. `test_lru_within_same_priority_alternates`：两条 priority=0 凭据，连续 N 次 acquire 应严格交替。
2. `test_newly_added_credential_picked_first`：先用一条凭据若干次，再加一条；下一次 acquire 必须选新加的。
3. `test_priority_grouping_falls_through_on_disable`：priority 0 一条 + priority 1 两条；禁用 priority 0，连续 acquire 应在两条 priority 1 之间 LRU 交替。
4. `test_opus_filter_within_priority_group`：priority 0 全是 Free 账号、priority 1 有 Pro；opus 请求直接落到 priority 1 组并组内 LRU。
5. `test_auto_disable_persists_credentials`：mock 一个`MAX_FAILURES_PER_CREDENTIAL` 触发的禁用，断言 `persist_credentials` 被调用（通过检查文件 mtime 或重新读文件状态）。

### 修改

- 所有断言 `current_id == X` 的测试改为：调用 `acquire_credential` 或 `acquire_context`，断言返回的 id 为 X。
- 删除 `loadBalancingMode` 相关的 5 个测试用例（priority 模式独占、balanced Least-Used、persist mode、invalid mode 校验等）。

### 不动

- `is_token_expired` 系列
- 凭据加载 / dup ID 检测
- region 选择逻辑
- API key 凭据完整性校验

## 向后兼容

| 文件 | 变化 | 兼容性 |
|---|---|---|
| `credentials.json` | 无字段变化 | 完全兼容 |
| `kiro_stats.json` | 无字段变化（`success_count` 字段保留） | 完全兼容 |
| `config.json` | `loadBalancingMode` 字段被忽略 | 旧配置可启动，启动 warn 一次 |

## 风险与缓解

- **并发选中冲突**：写锁内完成"挑选 + 打点"消除。
- **`last_used_at` 持久化频率**：每次 acquire 都触发 `save_stats_debounced`；现有节流策略（`STATS_SAVE_DEBOUNCE`）保证磁盘 I/O 不爆。
- **重启后初始顺序**：`kiro_stats.json` 已持久化 `last_used_at`，恢复后排序连续；未持久化的（新加凭据）= `None` = 最旧 = 优先选。
- **`ManagerSnapshot::current_id` 含义微变**：从"sticky"变成"最近被选中"。Admin UI 展示意图相同，但若有外部代码依赖"current_id 短期稳定"则需更新。

## 失败回滚

所有改动集中在 `src/kiro/token_manager.rs`、`src/admin/{service,handlers,router,types}.rs`、`src/model/config.rs`、`admin-ui`。
回滚 = `git revert`。无 schema 迁移、无外部依赖变更。
