//! 多场景集成测试：模拟真实多轮对话，验证 usage 三段拆分行为
//!
//! 不依赖上游 Kiro 凭据，直接组合 `count_all_tokens` + `ConvoTokenCache`
//! 来重现 handler 在响应中拼装 usage 时的逻辑。
//!
//! 用 `cargo test --bin kiro-rs cache_scenarios -- --nocapture` 跑可看到打印的数字。

#![cfg(test)]

use serde_json::json;

use super::prefix_cache::ConvoTokenCache;
use super::types::{Message, MessagesRequest, SystemMessage};
use crate::token::{count_all_tokens, count_message_tokens};

/// 把一次请求经过的 cache 拆分逻辑跑一遍，返回 (read, creation, input, total)
fn simulate_turn(
    cache: &ConvoTokenCache,
    conv_id: &str,
    req: &MessagesRequest,
) -> (i32, i32, i32, i32) {
    let total = count_all_tokens(
        req.model.clone(),
        req.system.clone(),
        req.messages.clone(),
        req.tools.clone(),
    ) as i32;
    let current_turn = req
        .messages
        .last()
        .map(|m| count_message_tokens(&m.content) as i32)
        .unwrap_or(0);
    let (read, creation, input) = cache.peek(conv_id, total, current_turn);
    cache.commit(conv_id, total, current_turn);
    (read, creation, input, total)
}

fn user(text: &str) -> Message {
    Message {
        role: "user".into(),
        content: json!(text),
    }
}

fn assistant(text: &str) -> Message {
    Message {
        role: "assistant".into(),
        content: json!(text),
    }
}

fn build_req(
    system: Option<&str>,
    messages: Vec<Message>,
) -> MessagesRequest {
    MessagesRequest {
        model: "claude-sonnet-4-5".into(),
        max_tokens: 1024,
        messages,
        stream: false,
        system: system.map(|s| vec![SystemMessage { text: s.into() }]),
        tools: None,
        tool_choice: None,
        thinking: None,
        output_config: None,
        metadata: None,
    }
}

fn print_header(name: &str) {
    println!("\n=== {} ===", name);
    println!(
        "{:>5}  {:>8}  {:>10}  {:>10}  {:>8}  {:>5}",
        "turn", "total", "creation", "read", "input", "sum_ok"
    );
}

fn print_row(turn: usize, read: i32, creation: i32, input: i32, total: i32) {
    let sum = read + creation + input;
    let ok = if sum == total { "✓" } else { "✗" };
    println!(
        "{:>5}  {:>8}  {:>10}  {:>10}  {:>8}  {:>5}",
        turn, total, creation, read, input, ok
    );
    assert_eq!(sum, total, "三段之和必须等于真实总输入");
}

const SHORT_SYS: &str = "You are a helpful assistant.";
const LONG_SYS: &str = "You are an expert software engineer assistant. \
You have deep knowledge of Rust, Python, JavaScript, TypeScript, Go, Java, and C++. \
When answering questions, provide accurate, idiomatic, and well-explained code. \
Cite documentation when relevant. Avoid speculation. \
If a question is ambiguous, ask for clarification before answering. \
Format code blocks with the appropriate language tag. \
When debugging, walk through the user's reasoning step by step. \
Always verify your assumptions before proposing fixes.";

#[test]
fn scenario_1_typical_short_chat() {
    print_header("场景 1: 短系统提示 + 5 轮英文对话（典型聊天）");
    let cache = ConvoTokenCache::new();
    let conv = "session-short-chat";

    let mut history = Vec::new();
    let user_msgs = [
        "Hi, what's 2+2?",
        "And 3 times that?",
        "Square that result.",
        "What's the cube root?",
        "Thanks, summarize what we just computed.",
    ];
    let asst_msgs = [
        "2+2 equals 4.",
        "3 times 4 is 12.",
        "12 squared is 144.",
        "The cube root of 144 is approximately 5.241.",
    ];

    for (i, q) in user_msgs.iter().enumerate() {
        history.push(user(q));
        let req = build_req(Some(SHORT_SYS), history.clone());
        let (r, c, inp, total) = simulate_turn(&cache, conv, &req);
        print_row(i + 1, r, c, inp, total);
        if i < asst_msgs.len() {
            history.push(assistant(asst_msgs[i]));
        }

        // 不变量：第一轮 cache_read 必为 0
        if i == 0 {
            assert_eq!(r, 0, "首轮不应有 cache_read");
        } else {
            // 续轮 cache_read 应该 >= 上一轮的 history（system + 之前所有消息）
            assert!(r > 0, "续轮 cache_read 必须 > 0");
        }
    }
}

#[test]
fn scenario_2_long_system_prompt() {
    print_header("场景 2: 长系统提示 + 4 轮对话（cache_read 应明显增大）");
    let cache = ConvoTokenCache::new();
    let conv = "session-long-sys";

    let mut history = Vec::new();
    let qa: &[(&str, &str)] = &[
        ("写一个 Rust 的 hello world", "println!(\"Hello, world!\");"),
        ("加上命令行参数解析", "use std::env::args; let args: Vec<_> = args().collect();"),
        ("把它改成异步的", "用 tokio::main 宏，main 函数加 async。"),
        ("加错误处理", "返回 anyhow::Result<()>。"),
    ];

    for (i, (q, a)) in qa.iter().enumerate() {
        history.push(user(q));
        let req = build_req(Some(LONG_SYS), history.clone());
        let (r, c, inp, total) = simulate_turn(&cache, conv, &req);
        print_row(i + 1, r, c, inp, total);
        history.push(assistant(a));
    }
}

#[test]
fn scenario_3_chinese_heavy() {
    print_header("场景 3: 中文为主的长对话（验证 token 计算系数）");
    let cache = ConvoTokenCache::new();
    let conv = "session-cn";

    let mut history = Vec::new();
    let qa: &[(&str, &str)] = &[
        (
            "请用通俗的语言解释一下什么是分布式系统的 CAP 定理，给个具体例子。",
            "CAP 定理指的是在分布式系统中一致性（Consistency）、可用性（Availability）、分区容错性（Partition Tolerance）三者只能取其二。比如选 CP 就是放弃部分可用性来保证强一致。",
        ),
        (
            "那 BASE 又是什么？和 CAP 是什么关系？",
            "BASE 是 Basically Available, Soft state, Eventually consistent，是 CAP 中选择 AP 的一种工程化实践。",
        ),
        (
            "举一个生产中真实使用 BASE 的系统例子，并说说它如何处理冲突。",
            "Cassandra 是典型例子。它通过 last-write-wins 或 vector clock 解决冲突。",
        ),
        (
            "如果业务对一致性要求很高，又必须容忍网络分区，应该怎么设计？",
            "可以引入 quorum 写、Raft/Paxos 共识、或者 saga 模式做最终补偿。",
        ),
    ];

    for (i, (q, a)) in qa.iter().enumerate() {
        history.push(user(q));
        let req = build_req(Some(LONG_SYS), history.clone());
        let (r, c, inp, total) = simulate_turn(&cache, conv, &req);
        print_row(i + 1, r, c, inp, total);
        history.push(assistant(a));
    }
}

#[test]
fn scenario_4_cross_session_isolation() {
    print_header("场景 4: 两个会话相互隔离（A 命中不应该让 B 命中）");
    let cache = ConvoTokenCache::new();

    let req_a = build_req(Some(SHORT_SYS), vec![user("Hello from A")]);
    let req_b = build_req(Some(SHORT_SYS), vec![user("Hello from B, with very different content")]);

    let (r_a1, c_a1, i_a1, t_a1) = simulate_turn(&cache, "conv-A", &req_a);
    print_row(1, r_a1, c_a1, i_a1, t_a1);
    println!("                ↑ conv-A 首轮");

    // 用 conv-B 发请求，cache_read 仍应该是 0
    let (r_b1, c_b1, i_b1, t_b1) = simulate_turn(&cache, "conv-B", &req_b);
    print_row(1, r_b1, c_b1, i_b1, t_b1);
    println!("                ↑ conv-B 首轮");
    assert_eq!(r_b1, 0, "另一个会话不应被 A 的提交污染");

    // A 的第二轮
    let req_a2 = build_req(
        Some(SHORT_SYS),
        vec![user("Hello from A"), assistant("Hi A"), user("follow-up")],
    );
    let (r_a2, c_a2, i_a2, t_a2) = simulate_turn(&cache, "conv-A", &req_a2);
    print_row(2, r_a2, c_a2, i_a2, t_a2);
    println!("                ↑ conv-A 第 2 轮（应命中）");
    assert!(r_a2 > 0, "A 的第二轮必须命中 A 的第一轮历史");
}

#[test]
fn scenario_5_history_shrinking() {
    print_header("场景 5: 客户端裁剪历史后再发（cache_read 被夹到本轮 history）");
    let cache = ConvoTokenCache::new();
    let conv = "session-shrink";

    // 第一轮：很长的历史（多对 QA）
    let long_history = vec![
        user("Q1 with some content here"),
        assistant("A1 detailed answer with explanations"),
        user("Q2 building on Q1"),
        assistant("A2 even longer explanation"),
        user("Q3 yet another question"),
    ];
    let req1 = build_req(Some(LONG_SYS), long_history.clone());
    let (r1, c1, i1, t1) = simulate_turn(&cache, conv, &req1);
    print_row(1, r1, c1, i1, t1);

    // 第二轮：客户端裁掉前面，只剩很短的历史
    let short_history = vec![
        user("Fresh start, much shorter context"),
    ];
    let req2 = build_req(Some(SHORT_SYS), short_history);
    let (r2, c2, i2, t2) = simulate_turn(&cache, conv, &req2);
    print_row(2, r2, c2, i2, t2);
    println!("                ↑ 历史被裁短，cache_read 不应超过本轮 history");
    let history_t2 = t2 - i2;
    assert!(r2 <= history_t2, "cache_read 不能超过当前 history");
}

#[test]
fn scenario_6_long_chat_growth() {
    print_header("场景 6: 10 轮长对话（命中率应稳步提高，cache_read 占比攀升）");
    let cache = ConvoTokenCache::new();
    let conv = "session-long";

    let mut history = Vec::new();
    for turn in 1..=10 {
        history.push(user(&format!(
            "这是第 {} 个问题，问题内容包含一些上下文，描述具体的技术细节。",
            turn
        )));
        let req = build_req(Some(LONG_SYS), history.clone());
        let (r, c, inp, total) = simulate_turn(&cache, conv, &req);
        let ratio = if total > 0 { r as f64 / total as f64 * 100.0 } else { 0.0 };
        println!(
            "{:>5}  {:>8}  {:>10}  {:>10}  {:>8}  cache_read 占比: {:>5.1}%",
            turn, total, c, r, inp, ratio
        );
        assert_eq!(r + c + inp, total);
        history.push(assistant(&format!("这是第 {} 个回答，包含详细的解释和示例代码。", turn)));
    }
}

#[test]
fn scenario_7_no_session_id_random_uuid() {
    print_header("场景 7: 无 session_id（每次都是新 UUID，永远命中 0）");
    let cache = ConvoTokenCache::new();

    let req = build_req(Some(LONG_SYS), vec![user("Hello"), assistant("Hi"), user("Again")]);
    for turn in 1..=3 {
        // 模拟 convert_request 在缺省时分配新 UUID
        let conv = uuid::Uuid::new_v4().to_string();
        let (r, c, inp, total) = simulate_turn(&cache, &conv, &req);
        print_row(turn, r, c, inp, total);
        assert_eq!(r, 0, "每次都是新 conv_id，永远不该命中");
    }
}

#[test]
fn scenario_8_ttl_expiry_simulated() {
    print_header("场景 8: 模拟 TTL 过期（用一个新 cache 实例代表 5min 后）");
    let cache_t1 = ConvoTokenCache::new();
    let conv = "session-ttl";

    let mut history = vec![user("Q1")];
    let req1 = build_req(Some(SHORT_SYS), history.clone());
    let (r1, c1, i1, t1) = simulate_turn(&cache_t1, conv, &req1);
    print_row(1, r1, c1, i1, t1);

    history.push(assistant("A1"));
    history.push(user("Q2"));
    let req2 = build_req(Some(SHORT_SYS), history.clone());
    let (r2, c2, i2, t2) = simulate_turn(&cache_t1, conv, &req2);
    print_row(2, r2, c2, i2, t2);
    println!("                ↑ 第 2 轮命中 first turn 的 history");

    // 模拟 5 分钟后：换一个全新的 cache 实例
    let cache_t2 = ConvoTokenCache::new();
    history.push(assistant("A2"));
    history.push(user("Q3"));
    let req3 = build_req(Some(SHORT_SYS), history.clone());
    let (r3, c3, i3, t3) = simulate_turn(&cache_t2, conv, &req3);
    print_row(3, r3, c3, i3, t3);
    println!("                ↑ 模拟 TTL 过期后，应回到 cache_read=0");
    assert_eq!(r3, 0, "TTL 过期后应该 miss");
}

#[test]
fn scenario_9_token_only_from_user_message() {
    print_header("场景 9: 各轮 input_tokens 只反映当前 user 消息（与历史长度无关）");
    let cache = ConvoTokenCache::new();
    let conv = "session-input-stable";

    // 当前 user 消息固定，但历史不断增长
    let fixed_q = "What's next?";
    let mut history = Vec::new();
    for turn in 1..=5 {
        history.push(user(fixed_q));
        let req = build_req(Some(LONG_SYS), history.clone());
        let (r, c, inp, total) = simulate_turn(&cache, conv, &req);
        print_row(turn, r, c, inp, total);
        history.push(assistant(&format!("Answer {}", turn)));
    }
    println!("                ↑ input_tokens 应基本稳定，total 应稳步上升");
}
